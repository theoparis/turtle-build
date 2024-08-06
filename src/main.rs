use clap::Parser;
use futures::future::try_join_all;
use std::{
    collections::HashMap,
    env::set_current_dir,
    path::{Path, PathBuf},
    process::exit,
    sync::Arc,
    time::Duration,
};
use tokio::time::sleep;
use turtle_build::arguments::{Arguments, Tool};
use turtle_build::ast::{Module, Statement};
use turtle_build::compile::compile;
use turtle_build::context::Context;
use turtle_build::error::ApplicationError;
use turtle_build::infrastructure::{OsCommandRunner, OsConsole, OsDatabase, OsFileSystem};
use turtle_build::module_dependency::ModuleDependencyMap;
use turtle_build::parse::parse;

const DEFAULT_BUILD_FILE: &str = "build.ninja";
const DATABASE_DIRECTORY: &str = ".turtle";
const OPEN_FILE_LIMIT: usize = if cfg!(target_os = "macos") { 256 } else { 1024 };
const DEFAULT_FILE_COUNT_PER_PROCESS: usize = 3; // stdin, stdout, and stderr

#[tokio::main]
async fn main() {
    let arguments = Arguments::parse();
    let job_limit = arguments.job_limit.unwrap_or_else(num_cpus::get);
    let context = Context::new(
        OsCommandRunner::new(job_limit),
        OsConsole::new(),
        OsDatabase::new(),
        OsFileSystem::new(
            OPEN_FILE_LIMIT
                .saturating_sub(DEFAULT_FILE_COUNT_PER_PROCESS * (job_limit + 1))
                .max(1),
        ),
    )
    .into();

    if let Err(error) = execute(&context, &arguments).await {
        if !arguments.quiet || !matches!(error, ApplicationError::Build) {
            context
                .console()
                .lock()
                .await
                .write_stderr(
                    format!(
                        "{}{}\n",
                        if let Some(prefix) = &arguments.log_prefix {
                            prefix
                        } else {
                            ""
                        },
                        error
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }

        // Delay for the error message to be written completely hopefully.
        sleep(Duration::from_millis(1)).await;

        exit(1)
    }
}

async fn execute(context: &Arc<Context>, arguments: &Arguments) -> Result<(), ApplicationError> {
    if let Some(directory) = &arguments.directory {
        set_current_dir(directory)?;
    }

    let root_module_path = context
        .file_system()
        .canonicalize_path(
            arguments
                .file
                .as_deref()
                .unwrap_or(DEFAULT_BUILD_FILE)
                .as_ref(),
        )
        .await?;
    let (modules, dependencies) = parse_modules(context, &root_module_path).await?;

    turtle_build::module_dependency::validate(&dependencies)?;

    let configuration = Arc::new(compile(&modules, &dependencies, &root_module_path)?);

    context.database().initialize(
        &configuration
            .build_directory()
            .map(|string| string.as_ref().as_ref())
            .unwrap_or_else(|| root_module_path.parent().unwrap())
            .join(DATABASE_DIRECTORY)
            .join(env!("CARGO_PKG_VERSION").replace('.', "_")),
    )?;

    if let Some(tool) = &arguments.tool {
        match tool {
            Tool::CleanDead => turtle_build::tool::clean_dead(context, &configuration).await?,
        }
    } else {
        turtle_build::run::run(
            context,
            configuration.clone(),
            &arguments.outputs,
            turtle_build::run::Options {
                debug: arguments.debug,
                profile: arguments.profile,
            },
        )
        .await?;
    }

    Ok(())
}

async fn parse_modules(
    context: &Context,
    path: &Path,
) -> Result<(HashMap<PathBuf, Module>, ModuleDependencyMap), ApplicationError> {
    let mut paths = vec![context.file_system().canonicalize_path(path).await?];
    let mut modules = HashMap::new();
    let mut dependencies = HashMap::new();

    while let Some(path) = paths.pop() {
        let mut source = String::new();

        context
            .file_system()
            .read_file_to_string(&path, &mut source)
            .await?;

        let module = parse(&source)?;

        let submodule_paths = try_join_all(
            module
                .statements()
                .iter()
                .filter_map(|statement| match statement {
                    Statement::Include(include) => Some(include.path()),
                    Statement::Submodule(submodule) => Some(submodule.path()),
                    _ => None,
                })
                .map(|submodule_path| resolve_submodule_path(context, &path, submodule_path))
                .collect::<Vec<_>>(),
        )
        .await?
        .into_iter()
        .collect::<HashMap<_, _>>();

        paths.extend(submodule_paths.values().cloned());

        modules.insert(path.clone(), module);
        dependencies.insert(path, submodule_paths);
    }

    Ok((modules, dependencies))
}

async fn resolve_submodule_path(
    context: &Context,
    module_path: &Path,
    submodule_path: &str,
) -> Result<(String, PathBuf), ApplicationError> {
    Ok((
        submodule_path.into(),
        context
            .file_system()
            .canonicalize_path(&module_path.parent().unwrap().join(submodule_path))
            .await?,
    ))
}
