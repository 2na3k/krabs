use super::config::SandboxConfig;
use crate::tools::tool::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// Wraps any `Tool` with sandbox enforcement:
///
/// - **`read`** / **`glob`** / **`grep`**: denied-read-path check on `path` arg
/// - **`write`**: allowed-write-path check on `path` arg
/// - **`bash`**: proxy env vars injected; on macOS also uses `sandbox-exec`
/// - All other tools: call passes through unchanged
pub struct SandboxedTool<T> {
    inner: T,
    config: Arc<SandboxConfig>,
    /// Port of the running `SandboxProxy`. Only relevant for bash / web tools.
    proxy_port: u16,
}

impl<T: Tool> SandboxedTool<T> {
    pub fn wrap(inner: T, config: Arc<SandboxConfig>, proxy_port: u16) -> Self {
        Self {
            inner,
            config,
            proxy_port,
        }
    }
}

#[async_trait]
impl<T: Tool> Tool for SandboxedTool<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.inner.parameters()
    }

    async fn call(&self, args: serde_json::Value) -> Result<ToolResult> {
        match self.inner.name() {
            // ── read-like tools: guard the `path` arg ──────────────────────
            "read" | "glob" | "grep" => {
                if let Some(path) = args["path"].as_str() {
                    if let Err(reason) = self.config.check_read_path(std::path::Path::new(path)) {
                        return Ok(ToolResult::err(reason));
                    }
                }
                self.inner.call(args).await
            }

            // ── write tool: guard the `path` arg ───────────────────────────
            "write" => {
                if let Some(path) = args["path"].as_str() {
                    if let Err(reason) = self.config.check_write_path(std::path::Path::new(path)) {
                        return Ok(ToolResult::err(reason));
                    }
                }
                self.inner.call(args).await
            }

            // ── bash: rewrite args to run via proxy (+ sandbox-exec on macOS)
            "bash" => self.call_bash(args).await,

            // ── everything else passes through ──────────────────────────────
            _ => self.inner.call(args).await,
        }
    }
}

impl<T: Tool> SandboxedTool<T> {
    async fn call_bash(&self, args: serde_json::Value) -> Result<ToolResult> {
        let command = match args["command"].as_str() {
            Some(c) => c.to_string(),
            None => return self.inner.call(args).await,
        };
        let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(30);
        let proxy_addr = format!("http://127.0.0.1:{}", self.proxy_port);

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.spawn_bash(&command, &proxy_addr),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Command timed out after {}s", timeout_secs))??;

        let mut content = String::new();
        if !output.stdout.is_empty() {
            content.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("stderr: ");
            content.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        let is_error = !output.status.success();
        if content.is_empty() {
            content = if is_error {
                format!("Command failed with exit code {:?}", output.status.code())
            } else {
                "(no output)".to_string()
            };
        }
        Ok(ToolResult { content, is_error })
    }

    async fn spawn_bash(&self, command: &str, proxy_addr: &str) -> Result<std::process::Output> {
        use tokio::process::Command;

        #[cfg(target_os = "macos")]
        {
            let profile = super::profile::build_profile(&self.config, self.proxy_port)?;
            let mut tmp = tempfile::NamedTempFile::new()?;
            use std::io::Write as _;
            tmp.write_all(profile.as_bytes())?;
            let profile_path = tmp.path().to_path_buf();
            let output = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("bash")
                .arg("-c")
                .arg(command)
                .env("http_proxy", proxy_addr)
                .env("https_proxy", proxy_addr)
                .output()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to execute command: {}", e))?;
            drop(tmp);
            Ok(output)
        }

        #[cfg(not(target_os = "macos"))]
        Command::new("bash")
            .arg("-c")
            .arg(command)
            .env("http_proxy", proxy_addr)
            .env("https_proxy", proxy_addr)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to execute command: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{SandboxConfig, SandboxProxy, SandboxedTool};
    use crate::tools::read::ReadTool;
    use crate::tools::tool::Tool;
    use crate::tools::write::WriteTool;
    use serde_json::json;

    async fn proxy_for(cfg: Arc<SandboxConfig>) -> (SandboxProxy, u16) {
        let proxy = SandboxProxy::start(Arc::clone(&cfg)).await.unwrap();
        let port = proxy.port();
        (proxy, port)
    }

    #[tokio::test]
    async fn sandboxed_read_blocks_denied_path() {
        let tmp = tempfile::tempdir().unwrap();
        let secret_dir = tmp.path().join("secrets");
        std::fs::create_dir_all(&secret_dir).unwrap();
        let secret_file = secret_dir.join("key");
        std::fs::write(&secret_file, "top-secret").unwrap();

        let cfg = Arc::new(SandboxConfig {
            enabled: true,
            denied_read_paths: vec![secret_dir.clone()],
            ..Default::default()
        });
        let (_proxy, port) = proxy_for(Arc::clone(&cfg)).await;
        let tool = SandboxedTool::wrap(ReadTool, cfg, port);

        let result: ToolResult = tool
            .call(json!({ "path": secret_file.to_str().unwrap() }))
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("sandbox"));
    }

    #[tokio::test]
    async fn sandboxed_read_passes_through_allowed_path() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("data.txt");
        std::fs::write(&file, "hello").unwrap();

        let cfg = Arc::new(SandboxConfig {
            enabled: true,
            ..Default::default()
        });
        let (_proxy, port) = proxy_for(Arc::clone(&cfg)).await;
        let tool = SandboxedTool::wrap(ReadTool, cfg, port);

        let result: ToolResult = tool
            .call(json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(result.content, "hello");
    }

    #[tokio::test]
    async fn sandboxed_write_blocks_outside_allowlist() {
        let cfg = Arc::new(SandboxConfig {
            enabled: true,
            allowed_write_paths: vec![std::path::PathBuf::from("/tmp")],
            ..Default::default()
        });
        let (_proxy, port) = proxy_for(Arc::clone(&cfg)).await;
        let tool = SandboxedTool::wrap(WriteTool, cfg, port);

        let result: ToolResult = tool
            .call(json!({ "path": "/etc/should_not_exist", "content": "bad" }))
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("sandbox"));
    }

    #[tokio::test]
    async fn sandboxed_write_allows_write_inside_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let cfg = Arc::new(SandboxConfig {
            enabled: true,
            ..Default::default()
        });
        let (_proxy, port) = proxy_for(Arc::clone(&cfg)).await;
        let tool = SandboxedTool::wrap(WriteTool, cfg, port);

        let target = tmp.path().join("output.txt");
        let result: ToolResult = tool
            .call(json!({ "path": target.to_str().unwrap(), "content": "ok" }))
            .await
            .unwrap();

        std::env::set_current_dir(prev).unwrap();
        assert!(!result.is_error, "{}", result.content);
    }

    #[tokio::test]
    async fn unsandboxed_read_passes_through() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("plain.txt");
        std::fs::write(&file, "world").unwrap();

        // sandbox disabled — use raw tool, no wrapper needed
        let tool = ReadTool;
        let result: ToolResult = tool
            .call(json!({ "path": file.to_str().unwrap() }))
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(result.content, "world");
    }
}
