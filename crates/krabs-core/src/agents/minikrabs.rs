use super::agent::{Agent, AgentOutput, KrabsAgent};
use anyhow::Result;
use std::sync::Arc;
use tokio::task::JoinHandle;

#[derive(Clone, Copy)]
pub enum SpawnMode {
    Process,
    Task,
}

pub struct MiniKrabsHandle {
    inner: HandleInner,
}

enum HandleInner {
    Task(JoinHandle<Result<AgentOutput>>),
}

impl MiniKrabsHandle {
    pub async fn join(self) -> Result<AgentOutput> {
        match self.inner {
            HandleInner::Task(handle) => handle
                .await
                .map_err(|e| anyhow::anyhow!("Task panicked: {}", e))?,
        }
    }
}

pub struct MiniKrabsSpawner {
    // Arc is justified here: the agent is shared across multiple spawned tasks.
    agent: Arc<KrabsAgent>,
}

impl MiniKrabsSpawner {
    pub fn new(agent: Arc<KrabsAgent>) -> Self {
        Self { agent }
    }

    pub async fn spawn(&self, task: &str, mode: SpawnMode) -> Result<MiniKrabsHandle> {
        match mode {
            SpawnMode::Process => match self.try_spawn_process(task).await {
                Ok(handle) => Ok(handle),
                Err(e) => {
                    tracing::warn!("Process spawn failed ({}), falling back to task mode", e);
                    self.spawn_task(task)
                }
            },
            SpawnMode::Task => self.spawn_task(task),
        }
    }

    async fn try_spawn_process(&self, task: &str) -> Result<MiniKrabsHandle> {
        let binary = which_krabs_cli()?;
        let tmp = std::env::temp_dir().join(format!("krabs-task-{}.json", uuid::Uuid::new_v4()));
        let task_json = serde_json::json!({
            "task": task,
            "config": {
                "model": self.agent.config.model,
                "base_url": self.agent.config.base_url,
                "api_key": self.agent.config.api_key,
                "max_turns": self.agent.config.max_turns,
            }
        });
        tokio::fs::write(&tmp, serde_json::to_string(&task_json)?).await?;

        let handle: JoinHandle<Result<AgentOutput>> = tokio::spawn(async move {
            let output = tokio::process::Command::new(&binary)
                .arg("--task-json")
                .arg(&tmp)
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to run krabs-cli: {}", e))?;

            tokio::fs::remove_file(&tmp).await.ok();

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("krabs-cli failed: {}", stderr);
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let result: AgentOutput = serde_json::from_str(&stdout)
                .map_err(|e| anyhow::anyhow!("Failed to parse output: {}", e))?;
            Ok(result)
        });

        Ok(MiniKrabsHandle {
            inner: HandleInner::Task(handle),
        })
    }

    fn spawn_task(&self, task: &str) -> Result<MiniKrabsHandle> {
        let agent = Arc::clone(&self.agent);
        let task = task.to_string();
        let handle = tokio::spawn(async move { agent.run(&task).await });
        Ok(MiniKrabsHandle {
            inner: HandleInner::Task(handle),
        })
    }

    pub async fn spawn_many(
        &self,
        tasks: Vec<String>,
        mode: SpawnMode,
    ) -> Vec<Result<AgentOutput>> {
        let mut handles = Vec::new();
        for task in &tasks {
            match self.spawn(task, mode).await {
                Ok(h) => handles.push(Ok(h)),
                Err(e) => handles.push(Err(e)),
            }
        }

        let mut results = Vec::new();
        for h in handles {
            match h {
                Ok(handle) => results.push(handle.join().await),
                Err(e) => results.push(Err(e)),
            }
        }
        results
    }
}

fn which_krabs_cli() -> Result<std::path::PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let candidate = std::path::Path::new(dir).join("krabs-cli");
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("krabs-cli not found in PATH")
}
