#[test]
fn cli_parses_core_flags() {
    use fetchling_cli::{parse_args, ParseOutcome};
    let out = parse_args([
        "fetchling",
        "-q",
        "-c",
        "--user-agent=test/1",
        "-O",
        "-",
        "http://127.0.0.1/x",
    ])
    .unwrap();
    match out {
        ParseOutcome::Run(c) => {
            assert!(c.quiet);
            assert!(c.continue_download);
            assert_eq!(c.user_agent, "test/1");
            assert_eq!(c.output_document.as_deref(), Some("-"));
        }
        _ => panic!("expected run"),
    }
}

#[test]
fn deferred_rejected() {
    use fetchling_cli::parse_args;
    use fetchling_core::Error;
    let err = parse_args(["fetchling", "--http2", "http://x"]).unwrap_err();
    assert!(matches!(err, Error::DeferredOption(_)));
}

#[test]
fn max_threads_parses() {
    use fetchling_cli::{parse_args, ParseOutcome};
    let out = parse_args(["fetchling", "--max-threads=2", "http://x"]).unwrap();
    match out {
        ParseOutcome::Run(c) => {
            assert_eq!(c.max_threads, 2);
            assert_eq!(c.max_threads_per_host, 2);
        }
        _ => panic!("expected run"),
    }
}

#[test]
fn max_threads_rejects_above_32() {
    use fetchling_cli::parse_args;
    use fetchling_core::Error;
    let err = parse_args(["fetchling", "--max-threads=33", "http://x"]).unwrap_err();
    assert!(matches!(err, Error::Parse(_)));
    assert!(err.to_string().contains("1..=32"));
}

#[test]
fn max_threads_allows_32() {
    use fetchling_cli::{parse_args, ParseOutcome};
    let out = parse_args(["fetchling", "--max-threads=32", "http://x"]).unwrap();
    match out {
        ParseOutcome::Run(c) => {
            assert_eq!(c.max_threads, 32);
            assert_eq!(c.max_threads_per_host, 4);
        }
        _ => panic!("expected run"),
    }
}

#[test]
fn max_threads_per_host_parses() {
    use fetchling_cli::{parse_args, ParseOutcome};
    let out = parse_args([
        "fetchling",
        "--max-threads=8",
        "--max-threads-per-host=8",
        "http://x",
    ])
    .unwrap();
    match out {
        ParseOutcome::Run(c) => {
            assert_eq!(c.max_threads, 8);
            assert_eq!(c.max_threads_per_host, 8);
        }
        _ => panic!("expected run"),
    }
}

#[test]
fn max_threads_per_host_rejects_above_32() {
    use fetchling_cli::parse_args;
    use fetchling_core::Error;
    let err = parse_args(["fetchling", "--max-threads-per-host=33", "http://x"]).unwrap_err();
    assert!(matches!(err, Error::Parse(_)));
    assert!(err.to_string().contains("1..=32"));
}

#[test]
fn max_threads_per_host_default_caps_at_4() {
    use fetchling_cli::{parse_args, ParseOutcome};
    let out = parse_args(["fetchling", "--max-threads=8", "http://x"]).unwrap();
    match out {
        ParseOutcome::Run(c) => assert_eq!(c.max_threads_per_host, 4),
        _ => panic!("expected run"),
    }
}

#[tokio::test]
async fn network_failure_preserves_network_exit_code() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_core::ExitCode;
    use fetchling_engine::Engine;

    // Port 1 is privileged/closed on typical hosts → connection failure.
    let out = parse_args([
        "fetchling",
        "-q",
        "--tries=1",
        "--timeout=1",
        "http://127.0.0.1:1/",
    ])
    .unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    let code = Engine::new(*cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Network);
}

#[test]
fn config_file_applies_before_cli() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("fetchling-smoke-rc-{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "quiet = on").unwrap();
        writeln!(f, "useragent = from-rc").unwrap();
    }
    let out = parse_args([
        "fetchling",
        &format!("--config={}", path.display()),
        "--user-agent=from-cli",
        "http://example.com/",
    ])
    .unwrap();
    match out {
        ParseOutcome::Run(c) => {
            assert!(c.quiet);
            assert_eq!(c.user_agent, "from-cli");
        }
        _ => panic!("expected run"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn base_flag_parses() {
    use fetchling_cli::{parse_args, ParseOutcome};
    let out = parse_args([
        "fetchling",
        "-B",
        "https://example.com/base/",
        "-i",
        "urls.txt",
        "http://unused/",
    ])
    .unwrap();
    match out {
        ParseOutcome::Run(c) => {
            assert_eq!(c.base.as_deref(), Some("https://example.com/base/"));
            assert_eq!(c.input_file.as_deref(), Some("urls.txt"));
        }
        _ => panic!("expected run"),
    }
}

#[tokio::test]
async fn timestamping_304_keeps_local_file() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_core::ExitCode;
    use fetchling_engine::Engine;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let req = String::from_utf8_lossy(&buf);
        assert!(
            req.to_ascii_lowercase().contains("if-modified-since:"),
            "expected IMS header in request: {req}"
        );
        let resp = b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(resp);
    });

    let dir = tempfile_dir();
    let dest = dir.join("stamp.bin");
    std::fs::write(&dest, b"original").unwrap();

    let out = parse_args([
        "fetchling",
        "-q",
        "-N",
        "--tries=1",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/stamp.bin"),
    ])
    .unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    let code = Engine::new(*cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&dest).unwrap(), b"original");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn save_headers_prepends_to_output() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_core::ExitCode;
    use fetchling_engine::Engine;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let resp = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody";
        let _ = stream.write_all(resp);
    });

    let dir = tempfile_dir();
    let dest = dir.join("headers.bin");
    let out = parse_args([
        "fetchling",
        "-q",
        "--save-headers",
        "--tries=1",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/x"),
    ])
    .unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    let code = Engine::new(*cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    let data = std::fs::read_to_string(&dest).unwrap();
    assert!(data.starts_with("HTTP/1.1 200 "));
    assert!(data.contains("content-type: text/plain\r\n"));
    assert!(data.contains("\r\n\r\nbody"));
    let _ = std::fs::remove_dir_all(&dir);
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fetchling-smoke-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn make_askpass(dir: &std::path::Path, password: &str) -> std::path::PathBuf {
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

#[tokio::test]
async fn use_askpass_sets_password_for_auth() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_core::ExitCode;
    use fetchling_engine::Engine;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.to_ascii_lowercase().contains("authorization: basic"),
            "expected basic auth: {req}"
        );
        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        let _ = stream.write_all(resp);
    });

    let dir = tempfile_dir();
    let dest = dir.join("auth.bin");
    let askpass = make_askpass(&dir, "s3cret");
    let out = parse_args([
        "fetchling",
        "-q",
        "--tries=1",
        "--user=alice",
        &format!("--use-askpass={}", askpass.display()),
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/x"),
    ])
    .unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    assert!(cfg.use_askpass.is_some());
    let code = Engine::new(*cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn warc_writes_request_record() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_core::ExitCode;
    use fetchling_engine::Engine;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody";
        let _ = stream.write_all(resp);
    });

    let dir = tempfile_dir();
    let dest = dir.join("out.bin");
    let warc = dir.join("out.warc");
    let out = parse_args([
        "fetchling",
        "-q",
        "--tries=1",
        "--no-warc-compression",
        &format!("--warc-file={}", warc.display()),
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/x"),
    ])
    .unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    let code = Engine::new(*cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    let text = std::fs::read_to_string(&warc).unwrap();
    assert!(text.contains("WARC-Type: request"));
    assert!(text.contains("WARC-Type: response"));
    assert!(
        text.contains("\r\n\r\nbody"),
        "response record should include body bytes: {text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn adjust_extension_renames_html() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_core::ExitCode;
    use fetchling_engine::Engine;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let body = b"<html>hi</html>";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.write_all(body);
    });

    let dir = tempfile_dir();
    let out = parse_args([
        "fetchling",
        "-q",
        "--tries=1",
        "-E",
        "-P",
        dir.to_str().unwrap(),
        &format!("http://{addr}/page"),
    ])
    .unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    let code = Engine::new(*cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    assert!(dir.join("page.html").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn local_encoding_unknown_rejected_at_run() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_engine::Engine;

    let out = parse_args([
        "fetchling",
        "--local-encoding=not-a-real-encoding",
        "http://example.com/",
    ])
    .unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    let err = Engine::new(*cfg)
        .unwrap()
        .run()
        .await
        .expect_err("unknown encoding should fail");
    assert!(err.to_string().contains("unknown character encoding"));
}

#[tokio::test]
async fn local_encoding_reads_latin1_input_file() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_core::ExitCode;
    use fetchling_engine::Engine;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(
            req.contains("caf%C3%A9") || req.contains("caf%c3%a9"),
            "expected percent-encoded path: {req}"
        );
        let body = b"ok";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.write_all(body);
    });

    let dir = tempfile_dir();
    let input = dir.join("urls.txt");
    // "http://ADDR/café" in ISO-8859-1 (é = 0xE9)
    let mut line = format!("http://{addr}/caf").into_bytes();
    line.push(0xE9);
    line.push(b'\n');
    std::fs::write(&input, line).unwrap();

    let out = parse_args([
        "fetchling",
        "-q",
        "--tries=1",
        "--local-encoding=ISO-8859-1",
        "-i",
        input.to_str().unwrap(),
        "-P",
        dir.to_str().unwrap(),
    ])
    .unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    let code = Engine::new(*cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn remote_encoding_and_no_iri_on_recurse() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_core::ExitCode;
    use fetchling_engine::Engine;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_bg = Arc::clone(&seen);
    thread::spawn(move || {
        for _ in 0..8 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            seen_bg.lock().unwrap().push(req.clone());
            let path = req.lines().next().unwrap_or("");
            let body: Vec<u8> = if path.contains("robots.txt") {
                b"User-agent: *\nAllow: /\n".to_vec()
            } else if path.contains("index") {
                let mut v = b"<html><a href=\"caf".to_vec();
                v.push(0xE9);
                v.extend_from_slice(b".html\">x</a></html>");
                v
            } else {
                b"child".to_vec()
            };
            let ct = if path.contains("robots.txt") {
                "text/plain"
            } else {
                "text/html; charset=ISO-8859-1"
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&body);
        }
    });

    let dir = tempfile_dir();
    let out = parse_args([
        "fetchling",
        "-q",
        "--tries=1",
        "-r",
        "-l",
        "1",
        "--remote-encoding=ISO-8859-1",
        "-P",
        dir.to_str().unwrap(),
        &format!("http://{addr}/index.html"),
    ])
    .unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    let code = Engine::new(*cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    let reqs = seen.lock().unwrap().clone();
    assert!(
        reqs.iter()
            .any(|r| r.contains("caf%C3%A9") || r.contains("caf%c3%a9")),
        "expected child fetch with percent-encoded IRI: {reqs:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn no_iri_rejects_non_ascii_cli_url() {
    use fetchling_cli::{parse_args, ParseOutcome};
    use fetchling_engine::Engine;

    let out = parse_args(["fetchling", "-q", "--no-iri", "http://example.com/café"]).unwrap();
    let ParseOutcome::Run(cfg) = out else {
        panic!("expected run");
    };
    let err = Engine::new(*cfg)
        .unwrap()
        .run()
        .await
        .expect_err("non-ascii should fail with --no-iri");
    assert!(err.to_string().contains("no-iri") || err.to_string().contains("non-ASCII"));
}
