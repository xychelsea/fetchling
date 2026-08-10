//! Shared localhost stubs for fetchling integration tests.

#![allow(dead_code)]

use fetchling_cli::{parse_args, ParseOutcome};
use fetchling_core::ExitCode;
use fetchling_engine::Engine;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fetchling-it-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn make_askpass(dir: &Path, password: &str) -> PathBuf {
    let path = dir.join("askpass");
    let script = format!("#!/usr/bin/env sh\nprintf '%s\\n' '{password}'\n");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    path
}

pub async fn run_fetchling<I, S>(args: I) -> ExitCode
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut argv = vec!["fetchling".to_string()];
    argv.extend(args.into_iter().map(|s| s.as_ref().to_string()));
    let out = parse_args(argv).expect("parse_args");
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected ParseOutcome::Run");
    };
    Engine::new(*cfg)
        .expect("Engine::new")
        .run()
        .await
        .expect("Engine::run")
}

pub async fn run_fetchling_result<I, S>(args: I) -> Result<ExitCode, fetchling_core::Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut argv = vec!["fetchling".to_string()];
    argv.extend(args.into_iter().map(|s| s.as_ref().to_string()));
    let out = parse_args(argv)?;
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected ParseOutcome::Run");
    };
    Engine::new(*cfg)?.run().await
}

/// Recorded HTTP requests (first line + raw bytes as UTF-8 lossy).
pub type RequestLog = Arc<Mutex<Vec<String>>>;

pub fn new_request_log() -> RequestLog {
    Arc::new(Mutex::new(Vec::new()))
}

#[derive(Clone)]
pub struct HttpResponse {
    pub status_line: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn ok(body: impl AsRef<[u8]>) -> Self {
        let body = body.as_ref().to_vec();
        Self {
            status_line: "HTTP/1.1 200 OK".into(),
            headers: vec![
                ("Content-Length".into(), body.len().to_string()),
                ("Connection".into(), "close".into()),
            ],
            body,
        }
    }

    pub fn html(body: impl AsRef<[u8]>) -> Self {
        let mut r = Self::ok(body);
        r.headers
            .insert(0, ("Content-Type".into(), "text/html".into()));
        r
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn status(mut self, status_line: &str) -> Self {
        self.status_line = status_line.into();
        self
    }

    fn write_to(&self, stream: &mut TcpStream) {
        let mut out = format!("{}\r\n", self.status_line);
        for (k, v) in &self.headers {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        let _ = stream.write_all(out.as_bytes());
        let _ = stream.write_all(&self.body);
    }
}

type HttpRouteHandler = Box<dyn Fn(&str) -> HttpResponse + Send>;

/// Route by path substring in the request line. First match wins; fallback is 404.
pub fn spawn_http_router(
    routes: Vec<(String, HttpRouteHandler)>,
    max_requests: usize,
    log: Option<RequestLog>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for _ in 0..max_requests {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 16384];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            if let Some(log) = &log {
                log.lock().unwrap().push(req.clone());
            }
            let path = req.lines().next().unwrap_or("");
            let mut matched = None;
            for (needle, handler) in &routes {
                if path.contains(needle.as_str()) {
                    matched = Some(handler(path));
                    break;
                }
            }
            let resp = matched.unwrap_or_else(|| HttpResponse {
                status_line: "HTTP/1.1 404 Not Found".into(),
                headers: vec![
                    ("Content-Length".into(), "0".into()),
                    ("Connection".into(), "close".into()),
                ],
                body: Vec::new(),
            });
            resp.write_to(&mut stream);
        }
    });
    addr
}

pub fn spawn_http_once(response: HttpResponse, log: Option<RequestLog>) -> SocketAddr {
    spawn_http_router(
        vec![("/".into(), Box::new(move |_| response.clone()))],
        1,
        log,
    )
}

/// Minimal PASV FTP server that serves a single file at `remote_path` (e.g. `/file.bin`).
pub fn spawn_ftp_file(remote_path: &str, contents: &[u8]) -> SocketAddr {
    let contents = contents.to_vec();
    let remote_path = remote_path.to_string();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((mut control, _)) = listener.accept() else {
            return;
        };
        let _ = control.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = control.write_all(b"220 fetchling test FTP\r\n");

        let mut data_listener: Option<TcpListener> = None;
        let mut buf = Vec::new();
        loop {
            let mut chunk = [0u8; 1024];
            let n = match control.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            buf.extend_from_slice(&chunk[..n]);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line = String::from_utf8_lossy(&buf[..=pos]).to_string();
                buf.drain(..=pos);
                let cmd = line.trim_end_matches(['\r', '\n']);
                let upper = cmd.to_ascii_uppercase();
                if upper.starts_with("USER ") {
                    let _ = control.write_all(b"331 Password required\r\n");
                } else if upper.starts_with("PASS ") {
                    let _ = control.write_all(b"230 Login ok\r\n");
                } else if upper.starts_with("TYPE ") {
                    let _ = control.write_all(b"200 Type set\r\n");
                } else if upper == "PASV" {
                    let dl = TcpListener::bind("127.0.0.1:0").unwrap();
                    let daddr = dl.local_addr().unwrap();
                    let port = daddr.port();
                    let p1 = port / 256;
                    let p2 = port % 256;
                    let reply = format!("227 Entering Passive Mode (127,0,0,1,{p1},{p2})\r\n");
                    let _ = control.write_all(reply.as_bytes());
                    data_listener = Some(dl);
                } else if upper.starts_with("SIZE ") {
                    let reply = format!("213 {}\r\n", contents.len());
                    let _ = control.write_all(reply.as_bytes());
                } else if upper.starts_with("RETR ") {
                    let path = cmd[5..].trim();
                    if path != remote_path.trim_start_matches('/')
                        && format!("/{path}") != remote_path
                        && path != remote_path
                    {
                        let _ = control.write_all(b"550 File not found\r\n");
                        continue;
                    }
                    let Some(dl) = data_listener.take() else {
                        let _ = control.write_all(b"425 No data connection\r\n");
                        continue;
                    };
                    let _ = control.write_all(b"150 Opening data connection\r\n");
                    if let Ok((mut data, _)) = dl.accept() {
                        let _ = data.write_all(&contents);
                    }
                    let _ = control.write_all(b"226 Transfer complete\r\n");
                } else if upper == "QUIT" {
                    let _ = control.write_all(b"221 Bye\r\n");
                    break;
                } else {
                    let _ = control.write_all(b"502 Command not implemented\r\n");
                }
            }
        }
    });
    addr
}
