use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SandboxConfig {
    pub enabled: bool,
    /// Paths the agent may write to (cwd always implicitly included)
    #[serde(default)]
    pub allowed_write_paths: Vec<PathBuf>,
    /// Paths blocked for reads (e.g. ~/.ssh, ~/.secrets)
    #[serde(default)]
    pub denied_read_paths: Vec<PathBuf>,
    /// Domain allowlist — empty = no allowlist enforced (only blocklist applies)
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Domains always blocked regardless of allowlist
    #[serde(default)]
    pub blocked_domains: Vec<String>,
}

impl SandboxConfig {
    /// Check if a path is allowed for reading.
    /// Returns `Err(String)` with a denial reason if blocked.
    pub fn check_read_path(&self, path: &std::path::Path) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| {
            if let Some(parent) = path.parent() {
                parent
                    .canonicalize()
                    .map(|p| p.join(path.file_name().unwrap_or_default()))
                    .unwrap_or_else(|_| path.to_path_buf())
            } else {
                path.to_path_buf()
            }
        });
        for denied in &self.denied_read_paths {
            let denied_canonical = denied
                .canonicalize()
                .unwrap_or_else(|_| denied.to_path_buf());
            if canonical.starts_with(&denied_canonical) {
                return Err(format!(
                    "sandbox: read denied for path {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    /// Check if a path is allowed for writing.
    /// Returns `Err(String)` with a denial reason if blocked.
    pub fn check_write_path(&self, path: &std::path::Path) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        // cwd is always implicitly allowed
        if let Ok(cwd) = std::env::current_dir() {
            let canonical = path.canonicalize().unwrap_or_else(|_| {
                // For new files, canonicalize parent
                if let Some(parent) = path.parent() {
                    parent
                        .canonicalize()
                        .map(|p| p.join(path.file_name().unwrap_or_default()))
                        .unwrap_or_else(|_| path.to_path_buf())
                } else {
                    path.to_path_buf()
                }
            });
            if canonical.starts_with(&cwd) {
                return Ok(());
            }
        }
        for allowed in &self.allowed_write_paths {
            let allowed_canonical = allowed
                .canonicalize()
                .unwrap_or_else(|_| allowed.to_path_buf());
            let canonical = path.canonicalize().unwrap_or_else(|_| {
                if let Some(parent) = path.parent() {
                    parent
                        .canonicalize()
                        .map(|p| p.join(path.file_name().unwrap_or_default()))
                        .unwrap_or_else(|_| path.to_path_buf())
                } else {
                    path.to_path_buf()
                }
            });
            if canonical.starts_with(&allowed_canonical) {
                return Ok(());
            }
        }
        Err(format!(
            "sandbox: write denied for path {}",
            path.display()
        ))
    }

    /// Check if a domain is allowed for network access.
    /// Returns `Err(String)` with a denial reason if blocked.
    pub fn check_domain(&self, domain: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        // Strip port if present
        let host = domain.split(':').next().unwrap_or(domain);

        // Explicit blocklist always wins
        for blocked in &self.blocked_domains {
            if host == blocked.as_str() || host.ends_with(&format!(".{}", blocked)) {
                return Err(format!("sandbox: domain {} is blocked", host));
            }
        }

        // If allowlist is non-empty, host must match
        if !self.allowed_domains.is_empty() {
            let allowed = self.allowed_domains.iter().any(|a| {
                host == a.as_str() || host.ends_with(&format!(".{}", a))
            });
            if !allowed {
                return Err(format!(
                    "sandbox: domain {} not in allowlist",
                    host
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn enabled() -> SandboxConfig {
        SandboxConfig {
            enabled: true,
            ..Default::default()
        }
    }

    // ── disabled sandbox ────────────────────────────────────────────────────

    #[test]
    fn disabled_sandbox_allows_everything() {
        let cfg = SandboxConfig::default(); // enabled = false
        assert!(cfg.check_read_path(Path::new("/etc/passwd")).is_ok());
        assert!(cfg.check_write_path(Path::new("/etc/passwd")).is_ok());
        assert!(cfg.check_domain("evil.example.com").is_ok());
    }

    // ── path / read guards ──────────────────────────────────────────────────

    #[test]
    fn read_denied_for_explicitly_blocked_path() {
        let tmp = tempfile::tempdir().unwrap();
        let secret = tmp.path().join("secrets");
        std::fs::create_dir_all(&secret).unwrap();

        let cfg = SandboxConfig {
            enabled: true,
            denied_read_paths: vec![secret.clone()],
            ..Default::default()
        };

        let target = secret.join("id_rsa");
        assert!(
            cfg.check_read_path(&target).is_err(),
            "should deny read inside denied dir"
        );
    }

    #[test]
    fn read_allowed_for_non_denied_path() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = SandboxConfig {
            enabled: true,
            denied_read_paths: vec![tmp.path().join("secrets")],
            ..Default::default()
        };
        assert!(cfg.check_read_path(Path::new("/tmp/some_other_file")).is_ok());
    }

    // ── path / write guards ─────────────────────────────────────────────────

    #[test]
    fn write_denied_outside_cwd_and_allowlist() {
        let cfg = SandboxConfig {
            enabled: true,
            allowed_write_paths: vec![PathBuf::from("/tmp")],
            ..Default::default()
        };
        // /etc is neither cwd nor in allowed_write_paths
        let result = cfg.check_write_path(Path::new("/etc/malicious"));
        assert!(result.is_err(), "should deny write to /etc");
    }

    #[test]
    fn write_allowed_to_explicit_allowed_path() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = SandboxConfig {
            enabled: true,
            allowed_write_paths: vec![tmp.path().to_path_buf()],
            ..Default::default()
        };
        let target = tmp.path().join("output.txt");
        assert!(cfg.check_write_path(&target).is_ok());
    }

    #[test]
    fn write_always_allowed_inside_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let cfg = enabled();
        let target = cwd.join("some_output.txt");
        assert!(
            cfg.check_write_path(&target).is_ok(),
            "cwd is always implicitly allowed"
        );
    }

    // ── domain / network guards ─────────────────────────────────────────────

    #[test]
    fn blocked_domain_is_denied() {
        let cfg = SandboxConfig {
            enabled: true,
            blocked_domains: vec!["evil.com".to_string()],
            ..Default::default()
        };
        assert!(cfg.check_domain("evil.com:443").is_err());
        assert!(cfg.check_domain("sub.evil.com:443").is_err());
    }

    #[test]
    fn blocklist_overrides_allowlist() {
        let cfg = SandboxConfig {
            enabled: true,
            allowed_domains: vec!["evil.com".to_string()],
            blocked_domains: vec!["evil.com".to_string()],
            ..Default::default()
        };
        assert!(
            cfg.check_domain("evil.com:443").is_err(),
            "blocklist must win over allowlist"
        );
    }

    #[test]
    fn allowlist_blocks_unlisted_domain() {
        let cfg = SandboxConfig {
            enabled: true,
            allowed_domains: vec!["api.openai.com".to_string()],
            ..Default::default()
        };
        assert!(cfg.check_domain("api.openai.com:443").is_ok());
        assert!(
            cfg.check_domain("github.com:443").is_err(),
            "github.com is not in allowlist"
        );
    }

    #[test]
    fn empty_allowlist_allows_all_non_blocked() {
        let cfg = SandboxConfig {
            enabled: true,
            blocked_domains: vec!["evil.com".to_string()],
            ..Default::default()
        };
        assert!(cfg.check_domain("github.com:443").is_ok());
        assert!(cfg.check_domain("api.openai.com:443").is_ok());
    }

    #[test]
    fn subdomain_matching_works() {
        let cfg = SandboxConfig {
            enabled: true,
            allowed_domains: vec!["openai.com".to_string()],
            ..Default::default()
        };
        assert!(cfg.check_domain("api.openai.com:443").is_ok());
        assert!(cfg.check_domain("chat.openai.com:443").is_ok());
        assert!(cfg.check_domain("openai.com:443").is_ok());
    }
}
