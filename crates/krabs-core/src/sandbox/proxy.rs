use super::config::SandboxConfig;
use anyhow::Result;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub struct SandboxProxy {
    port: u16,
    handle: JoinHandle<()>,
}

impl SandboxProxy {
    /// Start a CONNECT proxy on a random OS-assigned port.
    pub async fn start(config: Arc<SandboxConfig>) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        info!("SandboxProxy listening on 127.0.0.1:{}", port);

        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let cfg = Arc::clone(&config);
                        tokio::spawn(handle_connection(stream, cfg));
                    }
                    Err(e) => {
                        warn!("SandboxProxy accept error: {}", e);
                        break;
                    }
                }
            }
        });

        Ok(Self { port, handle })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for SandboxProxy {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn handle_connection(mut client: TcpStream, config: Arc<SandboxConfig>) {
    // Read the CONNECT request line by line
    let mut buf = vec![0u8; 4096];
    let n = match tokio::io::AsyncReadExt::read(&mut client, &mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = match std::str::from_utf8(&buf[..n]) {
        Ok(s) => s,
        Err(_) => return,
    };

    // Parse: CONNECT host:port HTTP/1.1
    let first_line = request.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 || parts[0] != "CONNECT" {
        return;
    }

    let target = parts[1];

    // Domain check
    if let Err(reason) = config.check_domain(target) {
        warn!("SandboxProxy blocking {}: {}", target, reason);
        let _ = client
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await;
        return;
    }

    // Connect to upstream
    let upstream = match TcpStream::connect(target).await {
        Ok(s) => s,
        Err(e) => {
            warn!("SandboxProxy failed to connect to {}: {}", target, e);
            let _ = client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await;
            return;
        }
    };

    // Send 200 Connection Established
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_err()
    {
        return;
    }

    info!("SandboxProxy tunneling to {}", target);

    // Bidirectional copy
    let (mut cr, mut cw) = client.into_split();
    let (mut ur, mut uw) = upstream.into_split();

    let client_to_upstream = tokio::io::copy(&mut cr, &mut uw);
    let upstream_to_client = tokio::io::copy(&mut ur, &mut cw);

    let _ = tokio::join!(client_to_upstream, upstream_to_client);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// Helper: send a raw CONNECT request to the proxy and read the first line.
    async fn connect_to_proxy(proxy_port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", proxy_port))
            .await
            .unwrap();
        let req = format!("CONNECT {} HTTP/1.1\r\nHost: {}\r\n\r\n", target, target);
        stream.write_all(req.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 256];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n])
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    }

    #[tokio::test]
    async fn proxy_starts_and_returns_a_port() {
        let cfg = Arc::new(SandboxConfig {
            enabled: true,
            ..Default::default()
        });
        let proxy = SandboxProxy::start(cfg).await.unwrap();
        assert!(proxy.port() > 0, "proxy should bind to a non-zero port");
    }

    #[tokio::test]
    async fn proxy_blocks_domain_on_blocklist() {
        let cfg = Arc::new(SandboxConfig {
            enabled: true,
            blocked_domains: vec!["blocked.example.com".to_string()],
            ..Default::default()
        });
        let proxy = SandboxProxy::start(cfg).await.unwrap();
        let response = connect_to_proxy(proxy.port(), "blocked.example.com:443").await;
        assert!(
            response.contains("403"),
            "expected 403 for blocked domain, got: {}",
            response
        );
    }

    #[tokio::test]
    async fn proxy_blocks_domain_not_in_allowlist() {
        let cfg = Arc::new(SandboxConfig {
            enabled: true,
            allowed_domains: vec!["api.openai.com".to_string()],
            ..Default::default()
        });
        let proxy = SandboxProxy::start(cfg).await.unwrap();
        let response = connect_to_proxy(proxy.port(), "github.com:443").await;
        assert!(
            response.contains("403"),
            "expected 403 for domain not in allowlist, got: {}",
            response
        );
    }

    #[tokio::test]
    async fn proxy_allows_domain_in_allowlist() {
        // We set up a tiny local TCP echo server to be the "upstream"
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = listener.local_addr().unwrap().port();

        // Accept one connection and immediately close it (simulates an upstream)
        tokio::spawn(async move {
            if let Ok((_, _)) = listener.accept().await {
                // accepted — just drop it
            }
        });

        let cfg = Arc::new(SandboxConfig {
            enabled: true,
            allowed_domains: vec!["localhost".to_string()],
            ..Default::default()
        });
        let proxy = SandboxProxy::start(cfg).await.unwrap();

        let response =
            connect_to_proxy(proxy.port(), &format!("localhost:{}", upstream_port)).await;
        assert!(
            response.contains("200"),
            "expected 200 for allowed domain, got: {}",
            response
        );
    }
}
