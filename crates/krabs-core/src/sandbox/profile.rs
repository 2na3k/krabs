/// macOS Seatbelt profile builder.
///
/// Only compiled and available on macOS. On other platforms this module
/// exposes no-op stubs so the rest of the codebase compiles unconditionally.
use super::config::SandboxConfig;

#[cfg(target_os = "macos")]
pub use macos::build_profile;

#[cfg(not(target_os = "macos"))]
pub use noop::build_profile;

#[cfg(target_os = "macos")]
mod macos {
    use super::SandboxConfig;
    use anyhow::Result;
    use std::fmt::Write;

    /// Generate a Seatbelt `.sb` profile string from `SandboxConfig` and the
    /// proxy port that bash subprocesses must use for outbound network access.
    pub fn build_profile(config: &SandboxConfig, proxy_port: u16) -> Result<String> {
        let mut sb = String::new();

        writeln!(sb, "(version 1)")?;
        writeln!(sb, "(deny default)")?;
        writeln!(sb)?;
        // Allow process execution and forking so bash can run commands
        writeln!(sb, "(allow process-exec process-fork)")?;
        writeln!(sb)?;
        // Allow all reads by default; denied_read_paths are enforced at the
        // application layer (ReadTool) for simplicity — Seatbelt read denials
        // for specific paths can be added here as a defense-in-depth measure.
        writeln!(sb, "(allow file-read*)")?;
        writeln!(sb)?;

        // Allow writes to cwd
        if let Ok(cwd) = std::env::current_dir() {
            writeln!(
                sb,
                "(allow file-write* (subpath \"{}\"))",
                cwd.display()
            )?;
        }

        // Allowed write paths from config
        for path in &config.allowed_write_paths {
            writeln!(
                sb,
                "(allow file-write* (subpath \"{}\"))",
                path.display()
            )?;
        }

        // Standard temp dirs that bash needs
        writeln!(sb, "(allow file-write* (subpath \"/tmp\"))")?;
        writeln!(sb, "(allow file-write* (subpath \"/var/folders\"))")?;
        writeln!(sb)?;

        // Network: only allow connections to the local proxy
        writeln!(
            sb,
            "(allow network-outbound (remote ip \"localhost:{}\"))",
            proxy_port
        )?;
        writeln!(
            sb,
            "(allow network-outbound (remote ip \"127.0.0.1:{}\"))",
            proxy_port
        )?;
        writeln!(sb, "(deny network-outbound)")?;
        writeln!(sb)?;

        // Allow sysctl reads (needed by many programs)
        writeln!(sb, "(allow sysctl-read)")?;
        // Allow signal delivery to own process group
        writeln!(sb, "(allow signal (target same-sandbox))")?;

        Ok(sb)
    }
}

#[cfg(not(target_os = "macos"))]
mod noop {
    use super::SandboxConfig;
    use anyhow::Result;

    /// On non-macOS platforms there is no Seatbelt; return an empty string.
    pub fn build_profile(_config: &SandboxConfig, _proxy_port: u16) -> Result<String> {
        Ok(String::new())
    }
}
