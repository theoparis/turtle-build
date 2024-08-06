use async_trait::async_trait;
use std::{error::Error, process::Output};
use tokio::{process::Command, sync::Semaphore};

#[async_trait]
pub trait CommandRunner {
    async fn run(&self, command: &str) -> Result<Output, Box<dyn Error>>;
}

#[derive(Debug)]
pub struct OsCommandRunner {
    semaphore: Semaphore,
}

impl OsCommandRunner {
    pub fn new(job_limit: usize) -> Self {
        Self {
            semaphore: Semaphore::new(job_limit),
        }
    }
}

#[async_trait]
impl CommandRunner for OsCommandRunner {
    async fn run(&self, command: &str) -> Result<Output, Box<dyn Error>> {
        let permit = self.semaphore.acquire().await?;

        let output = Command::new("nu").arg("-c").arg(command).output().await?;

        drop(permit);

        Ok(output)
    }
}
