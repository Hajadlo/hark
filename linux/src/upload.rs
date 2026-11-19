use crate::config::Config;
use anyhow::{Result, bail};
use reqwest::{
    Client, Request, StatusCode,
    multipart::{Form, Part},
};
use std::{
    sync::atomic::{AtomicU16, Ordering},
    time::Duration,
};
#[derive(Clone)]
pub struct Uploader {
    client: Client,
    config: Config,
    deadline: Duration,
}
pub fn retry(status: Option<u16>) -> bool {
    status.is_none() || status.is_some_and(|s| (500..600).contains(&s))
}
#[derive(Debug)]
struct Rejection {
    status: u16,
    message: String,
}
impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status, self.message)
    }
}
impl std::error::Error for Rejection {}
pub fn terminal(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<Rejection>()
        .is_some_and(|e| !retry(Some(e.status)))
}
impl Uploader {
    pub fn new(config: &Config) -> Result<Self> {
        Self::with_deadline(config, Duration::from_secs(120))
    }
    fn with_deadline(config: &Config, deadline: Duration) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(120))
                .timeout(deadline)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            config: config.clone(),
            deadline,
        })
    }
    pub fn request(&self, wav: Vec<u8>, mode: &str) -> Result<Request> {
        let form = Form::new()
            .part(
                "file",
                Part::bytes(wav)
                    .file_name("audio.wav")
                    .mime_str("audio/wav")?,
            )
            .text("model", mode.to_owned())
            .text("response_format", "json");
        Ok(self
            .client
            .post(format!("{}/v1/audio/transcriptions", self.config.url))
            .bearer_auth(&self.config.token)
            .header("X-Hark-Device", &self.config.device)
            .header(
                "User-Agent",
                concat!("hark-linux/", env!("CARGO_PKG_VERSION")),
            )
            .multipart(form)
            .build()?)
    }
    pub async fn transcribe(&self, wav: &[u8], mode: &str) -> Result<String> {
        // Keep a received terminal status even if the overall deadline expires
        // while reading its body; a 4xx must never turn into a retryable timeout.
        let observed = AtomicU16::new(0);
        match tokio::time::timeout(
            self.deadline,
            self.transcribe_attempts(wav, mode, &observed),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let status = observed.load(Ordering::Relaxed);
                if status != 0 && status != 200 && !retry(Some(status)) {
                    return Err(Rejection {
                        status,
                        message: if matches!(status,401|403) {
                            "check the token: server rejection; response body exceeded the upload deadline".into()
                        } else { "server rejection; response body exceeded the upload deadline".into() },
                    }
                    .into());
                }
                Err(anyhow::anyhow!(
                    "network timeout: upload overall deadline exceeded"
                ))
            }
        }
    }
    async fn transcribe_attempts(
        &self,
        wav: &[u8],
        mode: &str,
        observed: &AtomicU16,
    ) -> Result<String> {
        for attempt in 0..3 {
            observed.store(0, Ordering::Relaxed);
            let response = self.client.execute(self.request(wav.to_vec(), mode)?).await;
            let failure = match response {
                Ok(r) => {
                    let status = r.status();
                    observed.store(status.as_u16(), Ordering::Relaxed);
                    // A terminal status stays terminal even if its body is unreadable.
                    if status != StatusCode::OK && !retry(Some(status.as_u16())) {
                        let body = r.bytes().await.ok();
                        let message = body
                            .as_deref()
                            .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                            .and_then(|j| j["error"]["message"].as_str().map(str::to_owned))
                            .unwrap_or_else(|| format!("server rejected transcription ({status})"));
                        let message = self.config.redact(&message);
                        let message = if matches!(status.as_u16(), 401 | 403) {
                            format!("check the token: {message}")
                        } else {
                            message
                        };
                        return Err(Rejection {
                            status: status.as_u16(),
                            message,
                        }
                        .into());
                    }
                    match r.bytes().await {
                        Ok(body) => {
                            if status == StatusCode::OK {
                                let json: serde_json::Value = serde_json::from_slice(&body)
                                    .map_err(|_| anyhow::anyhow!("invalid transcription JSON"))?;
                                return json["text"].as_str().map(str::to_owned).ok_or_else(|| {
                                    anyhow::anyhow!("transcription has no text field")
                                });
                            }
                            let message = serde_json::from_slice::<serde_json::Value>(&body)
                                .ok()
                                .and_then(|j| j["error"]["message"].as_str().map(str::to_owned))
                                .unwrap_or_else(|| format!("server returned HTTP {status}"));
                            self.config.redact(&message)
                        }
                        Err(_) => "network error reading response".into(),
                    }
                }
                Err(_) => "network error or timeout contacting server".into(),
            };
            if attempt == 2 {
                bail!("{failure}");
            }
            tokio::time::sleep(Duration::from_secs(attempt + 1)).await;
        }
        unreachable!()
    }
    pub async fn models(&self) -> Result<serde_json::Value> {
        let r = self
            .client
            .get(format!("{}/v1/models", self.config.url))
            .header("X-Hark-Web", "1")
            .bearer_auth(&self.config.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("models request failed"))?;
        let status = r.status();
        if !status.is_success() {
            bail!("models HTTP {status}; check token and Tailscale owner access");
        }
        r.json()
            .await
            .map_err(|_| anyhow::anyhow!("invalid models JSON"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_contract() {
        for s in [400, 401, 403, 404, 411, 413, 422, 429] {
            assert!(!retry(Some(s)));
        }
        for s in [500, 502, 503, 599] {
            assert!(retry(Some(s)));
        }
        assert!(retry(None));
    }
    #[tokio::test]
    async fn terminal_status_remains_terminal_when_body_stalls_past_deadline() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::mpsc,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = Config {
            url: format!("http://{}", listener.local_addr().unwrap()),
            token: "x".repeat(32),
            device: "test".into(),
            ..Config::default()
        };
        let (release, hold) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = vec![];
            let mut buf = [0; 4096];
            let end = loop {
                let n = stream.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(p) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
            let len: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            while bytes.len() < end + len {
                let n = stream.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
            }
            write!(
                stream,
                "HTTP/1.1 422 Rejected\r\nContent-Length: 100\r\n\r\n"
            )
            .unwrap();
            let _ = hold.recv_timeout(Duration::from_secs(2));
        });
        let error = Uploader::with_deadline(&config, Duration::from_millis(150))
            .unwrap()
            .transcribe(b"RIFF test", "raw")
            .await
            .unwrap_err();
        assert!(terminal(&error));
        assert_eq!(error.downcast_ref::<Rejection>().unwrap().status, 422);
        drop(release);
        server.join().unwrap();
    }
    #[tokio::test]
    async fn stalled_request_body_write_has_an_overall_deadline() {
        use std::{net::TcpListener, sync::mpsc, time::Instant};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = Config {
            url: format!("http://{}", listener.local_addr().unwrap()),
            token: "x".repeat(32),
            device: "test".into(),
            ..Config::default()
        };
        let (release, hold) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            // Deliberately never read the request body; no response read can start.
            let _ = hold.recv_timeout(Duration::from_secs(3));
            drop(socket);
        });
        let uploader = Uploader::with_deadline(&config, Duration::from_millis(100)).unwrap();
        let started = Instant::now();
        let error = uploader
            .client
            .execute(uploader.request(vec![0; 16 * 1024 * 1024], "raw").unwrap())
            .await
            .unwrap_err();
        assert!(error.is_timeout());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!terminal(&error.into()));
        drop(release);
        server.join().unwrap();
    }
    #[tokio::test]
    async fn terminal_status_keeps_classification_even_with_truncated_body() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        for (status, truncated) in [(401, false), (422, false), (429, false), (422, true)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let config = Config {
                url: format!("http://{}", listener.local_addr().unwrap()),
                token: "x".repeat(32),
                device: "test".into(),
                ..Config::default()
            };
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = vec![];
                let mut buf = [0; 4096];
                let end = loop {
                    let n = stream.read(&mut buf).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(p) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let len: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                while bytes.len() < end + len {
                    let n = stream.read(&mut buf).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buf[..n]);
                }
                let body = r#"{"error":{"message":"synthetic rejection"}}"#;
                write!(stream,"HTTP/1.1 {status} Rejected\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()+if truncated {100} else {0}).unwrap();
                // Dropping the listener ensures an erroneous retry becomes a network
                // error rather than another terminal response that masks the retry.
            });
            let error = Uploader::new(&config)
                .unwrap()
                .transcribe(b"RIFF test", "raw")
                .await
                .unwrap_err();
            assert!(terminal(&error), "{status}, truncated={truncated}: {error}");
            assert_eq!(error.downcast_ref::<Rejection>().unwrap().status, status);
            if status == 401 {
                assert!(error.to_string().contains("check the token"));
            }
            server.join().unwrap();
        }
    }
    #[tokio::test]
    async fn multipart_headers_fields_length() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let config = Config {
            url: format!("http://{address}"),
            token: "x".repeat(32),
            device: "test".into(),
            ..Config::default()
        };
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = vec![];
            let mut buf = [0; 4096];
            let end = loop {
                let n = stream.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(p) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            while bytes.len() < end + length {
                let n = stream.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
            }
            assert!(headers.contains("authorization: bearer xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
            assert!(headers.contains("x-hark-device: test"));
            assert!(headers.contains("user-agent: hark-linux/0.1.0"));
            assert!(!headers.contains("transfer-encoding"));
            let body = String::from_utf8_lossy(&bytes[end..]);
            assert!(body.contains("name=\"file\"; filename=\"audio.wav\""));
            assert!(body.contains("Content-Type: audio/wav"));
            assert!(body.contains("name=\"model\"\r\n\r\nraw"));
            assert!(body.contains("name=\"response_format\"\r\n\r\njson"));
            assert_eq!(bytes.len() - end, length);
            let body = r#"{"text":"synthetic text"}"#;
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        assert_eq!(
            Uploader::new(&config)
                .unwrap()
                .transcribe(b"RIFF synthetic", "raw")
                .await
                .unwrap(),
            "synthetic text"
        );
        server.join().unwrap();
    }
}
