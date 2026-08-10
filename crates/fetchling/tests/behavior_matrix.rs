//! Behavior matrix covering advertised README Features (localhost only).

mod common;

use common::{
    make_askpass, new_request_log, run_fetchling, spawn_ftp_file, spawn_http_once,
    spawn_http_router, tempfile_dir, HttpResponse,
};
use fetchling_core::ExitCode;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn single_file_http_download() {
    let addr = spawn_http_once(HttpResponse::ok(b"hello-world"), None);
    let dir = tempfile_dir();
    let dest = dir.join("out.bin");
    let code = run_fetchling([
        "-q",
        "--tries=1",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/file.bin"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&dest).unwrap(), b"hello-world");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn resume_continue_sends_range_and_appends() {
    let log = new_request_log();
    let addr = spawn_http_once(
        HttpResponse {
            status_line: "HTTP/1.1 206 Partial Content".into(),
            headers: vec![
                ("Content-Length".into(), "5".into()),
                ("Content-Range".into(), "bytes 5-9/10".into()),
                ("Connection".into(), "close".into()),
            ],
            body: b"WORLD".to_vec(),
        },
        Some(Arc::clone(&log)),
    );
    let dir = tempfile_dir();
    let dest = dir.join("partial.bin");
    std::fs::write(&dest, b"HELLO").unwrap();
    let code = run_fetchling([
        "-q",
        "-c",
        "--tries=1",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/partial.bin"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    let req = log.lock().unwrap().join("\n");
    assert!(
        req.to_ascii_lowercase().contains("range: bytes=5-"),
        "expected Range header: {req}"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"HELLOWORLD");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn redirect_follow_fetches_final_body() {
    let log = new_request_log();
    let addr_holder: Arc<Mutex<Option<std::net::SocketAddr>>> = Arc::new(Mutex::new(None));
    // Bind first so we know the addr for Location; use a two-step router on one listener.
    let log2 = Arc::clone(&log);
    let addr = spawn_http_router(
        vec![
            (
                "/final".into(),
                Box::new(|_| HttpResponse::ok(b"final-body")),
            ),
            (
                "/start".into(),
                Box::new(move |_| {
                    // Location filled after bind via closure over shared addr — see below
                    HttpResponse {
                        status_line: "HTTP/1.1 302 Found".into(),
                        headers: vec![
                            ("Content-Length".into(), "0".into()),
                            ("Connection".into(), "close".into()),
                            ("Location".into(), "/final".into()),
                        ],
                        body: Vec::new(),
                    }
                }),
            ),
        ],
        4,
        Some(log2),
    );
    *addr_holder.lock().unwrap() = Some(addr);

    let dir = tempfile_dir();
    let dest = dir.join("redir.bin");
    let code = run_fetchling([
        "-q",
        "--tries=1",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/start"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&dest).unwrap(), b"final-body");
    let reqs = log.lock().unwrap().clone();
    assert!(
        reqs.iter().any(|r| r.contains("/start")),
        "expected /start: {reqs:?}"
    );
    assert!(
        reqs.iter().any(|r| r.contains("/final")),
        "expected /final: {reqs:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cookies_load_sends_cookie_header() {
    let dir = tempfile_dir();
    let cookie_jar = dir.join("cookies.txt");
    // Netscape cookie for 127.0.0.1 (hand-written; IP host-only cookies often omit Domain on save).
    std::fs::write(
        &cookie_jar,
        "# Netscape HTTP Cookie File\n\
         127.0.0.1\tFALSE\t/\tFALSE\t0\tsid\tabc\n",
    )
    .unwrap();

    let log = new_request_log();
    let addr = spawn_http_once(HttpResponse::ok(b"two"), Some(Arc::clone(&log)));
    let dest = dir.join("b.bin");
    let code = run_fetchling([
        "-q",
        "--tries=1",
        &format!("--load-cookies={}", cookie_jar.display()),
        "--keep-session-cookies",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/b"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    let req = log.lock().unwrap().join("\n");
    assert!(
        req.to_ascii_lowercase().contains("cookie:") && req.contains("sid=abc"),
        "expected Cookie header from jar: {req}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cookies_save_writes_jar_file() {
    let dir = tempfile_dir();
    let cookie_jar = dir.join("cookies.txt");
    let dest = dir.join("a.bin");
    let addr = spawn_http_once(
        HttpResponse::ok(b"one").with_header("Set-Cookie", "sid=abc; Path=/; Max-Age=3600"),
        None,
    );
    let code = run_fetchling([
        "-q",
        "--tries=1",
        &format!("--save-cookies={}", cookie_jar.display()),
        "--keep-session-cookies",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/a"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    assert!(
        cookie_jar.is_file(),
        "expected --save-cookies to create {}",
        cookie_jar.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn basic_auth_sends_authorization_header() {
    let log = new_request_log();
    let addr = spawn_http_once(HttpResponse::ok(b"ok"), Some(Arc::clone(&log)));
    let dir = tempfile_dir();
    let dest = dir.join("auth.bin");
    let askpass = make_askpass(&dir, "s3cret");
    let code = run_fetchling([
        "-q",
        "--tries=1",
        "--user=alice",
        &format!("--use-askpass={}", askpass.display()),
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/x"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    let req = log.lock().unwrap().join("\n");
    assert!(
        req.to_ascii_lowercase().contains("authorization: basic"),
        "expected Basic auth: {req}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn http_proxy_uses_absolute_form() {
    let log = new_request_log();
    let proxy = spawn_http_once(HttpResponse::ok(b"via-proxy"), Some(Arc::clone(&log)));
    let dir = tempfile_dir();
    let dest = dir.join("proxied.bin");
    // Origin need not exist; proxy synthesizes the response. Absolute-form target is asserted.
    let code = run_fetchling([
        "-q",
        "--tries=1",
        &format!("--http-proxy=http://{proxy}"),
        "-O",
        dest.to_str().unwrap(),
        "http://example.invalid/through-proxy",
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&dest).unwrap(), b"via-proxy");
    let req = log.lock().unwrap().join("\n");
    assert!(
        req.contains("GET http://example.invalid/through-proxy"),
        "expected absolute-form request: {req}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn recurse_respects_robots_deny() {
    let log = new_request_log();
    let addr = spawn_http_router(
        vec![
            (
                "robots.txt".into(),
                Box::new(|_| HttpResponse::ok(b"User-agent: *\nDisallow: /secret\nAllow: /\n")),
            ),
            (
                "/index".into(),
                Box::new(|_| {
                    HttpResponse::html(
                        b"<html><a href=\"/secret\">no</a><a href=\"/ok\">yes</a></html>",
                    )
                }),
            ),
            ("/secret".into(), Box::new(|_| HttpResponse::ok(b"denied"))),
            ("/ok".into(), Box::new(|_| HttpResponse::ok(b"allowed"))),
        ],
        16,
        Some(Arc::clone(&log)),
    );
    let dir = tempfile_dir();
    let code = run_fetchling([
        "-q",
        "--tries=1",
        "-r",
        "-l",
        "1",
        "-P",
        dir.to_str().unwrap(),
        &format!("http://{addr}/index.html"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    let reqs = log.lock().unwrap().clone();
    assert!(
        reqs.iter().any(|r| r.contains("/ok")),
        "expected /ok fetch: {reqs:?}"
    );
    assert!(
        !reqs.iter().any(|r| r.contains("GET /secret")),
        "robots should block /secret: {reqs:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn page_requisites_fetches_linked_asset() {
    let log = new_request_log();
    let addr = spawn_http_router(
        vec![
            (
                "robots.txt".into(),
                Box::new(|_| HttpResponse::ok(b"User-agent: *\nAllow: /\n")),
            ),
            (
                "/page".into(),
                Box::new(|_| HttpResponse::html(b"<html><img src=\"/asset.png\"></html>")),
            ),
            (
                "/asset.png".into(),
                Box::new(|_| HttpResponse::ok(b"PNG").with_header("Content-Type", "image/png")),
            ),
        ],
        16,
        Some(Arc::clone(&log)),
    );
    let dir = tempfile_dir();
    let code = run_fetchling([
        "-q",
        "--tries=1",
        "-p",
        "-P",
        dir.to_str().unwrap(),
        &format!("http://{addr}/page.html"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    let reqs = log.lock().unwrap().clone();
    assert!(
        reqs.iter().any(|r| r.contains("/asset.png")),
        "expected page-requisite asset fetch: {reqs:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn pasv_ftp_download() {
    let addr = spawn_ftp_file("/hello.bin", b"ftp-bytes");
    let dir = tempfile_dir();
    let dest = dir.join("hello.bin");
    let code = run_fetchling([
        "-q",
        "--tries=1",
        "-O",
        dest.to_str().unwrap(),
        &format!("ftp://{addr}/hello.bin"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&dest).unwrap(), b"ftp-bytes");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deferred_still_rejected() {
    use fetchling_cli::parse_args;
    use fetchling_core::Error;
    let err = parse_args(["fetchling", "--http2", "http://x"]).unwrap_err();
    assert!(matches!(err, Error::DeferredOption(_)));
}

#[test]
fn wgetrc_robots_command_parses() {
    use fetchling_cli::{parse_args, ParseOutcome};
    let out = parse_args([
        "fetchling",
        "-e",
        "robots = off",
        "--tries=1",
        "http://127.0.0.1/x",
    ])
    .unwrap();
    assert!(matches!(out, ParseOutcome::Run(_)));
}
