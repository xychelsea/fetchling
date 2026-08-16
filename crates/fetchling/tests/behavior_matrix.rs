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

#[tokio::test]
async fn gzip_content_encoding_decodes_body() {
    let gzip = [
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0xd7, 0x4d, 0xaf, 0xca, 0x2c, 0x00, 0x00, 0xa8, 0xae, 0x42, 0x27, 0x0a, 0x00, 0x00, 0x00,
    ];
    let addr = spawn_http_once(
        HttpResponse::ok(gzip).with_header("Content-Encoding", "gzip"),
        None,
    );
    let dir = tempfile_dir();
    let dest = dir.join("out.bin");
    let code = run_fetchling([
        "--no-config",
        "-q",
        "--tries=1",
        "--compression=gzip",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/file.bin"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&dest).unwrap(), b"hello-gzip");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn header_and_user_agent_sent() {
    let log = new_request_log();
    let addr = spawn_http_once(HttpResponse::ok(b"ok"), Some(Arc::clone(&log)));
    let dir = tempfile_dir();
    let dest = dir.join("out.bin");
    let code = run_fetchling([
        "--no-config",
        "-q",
        "--tries=1",
        "--header=X-Test: 1",
        "-U",
        "fetchling-it/1",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/x"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    let req = log.lock().unwrap().join("\n");
    let lower = req.to_ascii_lowercase();
    assert!(
        lower.contains("x-test: 1") && lower.contains("user-agent: fetchling-it/1"),
        "expected custom header and UA: {req}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn post_data_sends_post_body() {
    let log = new_request_log();
    let addr = spawn_http_once(HttpResponse::ok(b"ok"), Some(Arc::clone(&log)));
    let dir = tempfile_dir();
    let dest = dir.join("out.bin");
    let code = run_fetchling([
        "--no-config",
        "-q",
        "--tries=1",
        "--post-data=a=b",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/x"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    let req = log.lock().unwrap().join("\n");
    assert!(
        req.starts_with("POST ") || req.contains("POST /"),
        "expected POST: {req}"
    );
    assert!(req.contains("a=b"), "expected POST body: {req}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn no_clobber_leaves_existing_dest() {
    let addr = spawn_http_once(HttpResponse::ok(b"new-bytes"), None);
    let dir = tempfile_dir();
    let dest = dir.join("keep.bin");
    std::fs::write(&dest, b"keep-me").unwrap();
    let code = run_fetchling([
        "--no-config",
        "-q",
        "--tries=1",
        "-nc",
        "-P",
        dir.to_str().unwrap(),
        &format!("http://{addr}/keep.bin"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep-me");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn recurse_accept_html_skips_png() {
    let log = new_request_log();
    let addr = spawn_http_router(
        vec![
            (
                "robots.txt".into(),
                Box::new(|_| HttpResponse::ok(b"User-agent: *\nAllow: /\n")),
            ),
            (
                "/index".into(),
                Box::new(|_| {
                    HttpResponse::html(
                        b"<html><a href=\"ok.html\">y</a><a href=\"skip.png\">n</a></html>",
                    )
                }),
            ),
            ("/ok.html".into(), Box::new(|_| HttpResponse::ok(b"ok"))),
            ("/skip.png".into(), Box::new(|_| HttpResponse::ok(b"png"))),
        ],
        16,
        Some(Arc::clone(&log)),
    );
    let dir = tempfile_dir();
    let code = run_fetchling([
        "--no-config",
        "-q",
        "--tries=1",
        "-r",
        "-l",
        "1",
        "-A",
        "*.html",
        "-P",
        dir.to_str().unwrap(),
        &format!("http://{addr}/index.html"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    let reqs = log.lock().unwrap().clone();
    assert!(
        reqs.iter().any(|r| r.contains("/ok.html")),
        "expected /ok.html: {reqs:?}"
    );
    assert!(
        !reqs.iter().any(|r| r.contains("skip.png")),
        "accept filter should skip png: {reqs:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn recurse_follows_css_url() {
    let log = new_request_log();
    let addr = spawn_http_router(
        vec![
            (
                "robots.txt".into(),
                Box::new(|_| HttpResponse::ok(b"User-agent: *\nAllow: /\n")),
            ),
            (
                "/page".into(),
                Box::new(|_| HttpResponse::html(b"<html><link href=\"style.css\"></html>")),
            ),
            (
                "/style.css".into(),
                Box::new(|_| {
                    HttpResponse::ok(b"body{background:url(asset.bin);}")
                        .with_header("Content-Type", "text/css")
                }),
            ),
            (
                "/asset.bin".into(),
                Box::new(|_| HttpResponse::ok(b"ASSET")),
            ),
        ],
        16,
        Some(Arc::clone(&log)),
    );
    let dir = tempfile_dir();
    let code = run_fetchling([
        "--no-config",
        "-q",
        "--tries=1",
        "-r",
        "-l",
        "2",
        "-P",
        dir.to_str().unwrap(),
        &format!("http://{addr}/page.html"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    let reqs = log.lock().unwrap().clone();
    assert!(
        reqs.iter().any(|r| r.contains("asset.bin")),
        "expected CSS url() fetch: {reqs:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn retries_http_503_then_succeeds() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let n = Arc::new(AtomicU32::new(0));
    let n2 = Arc::clone(&n);
    let addr = spawn_http_router(
        vec![(
            "/".into(),
            Box::new(move |_| {
                if n2.fetch_add(1, Ordering::SeqCst) == 0 {
                    HttpResponse::ok(b"").status("HTTP/1.1 503 Service Unavailable")
                } else {
                    HttpResponse::ok(b"after-retry")
                }
            }),
        )],
        4,
        None,
    );
    let dir = tempfile_dir();
    let dest = dir.join("out.bin");
    let code = run_fetchling([
        "--no-config",
        "-q",
        "--tries=2",
        "--waitretry=0",
        "-O",
        dest.to_str().unwrap(),
        &format!("http://{addr}/file.bin"),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&dest).unwrap(), b"after-retry");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn input_file_fetches_both_urls() {
    let addr = spawn_http_router(
        vec![
            ("/a.bin".into(), Box::new(|_| HttpResponse::ok(b"aaa"))),
            ("/b.bin".into(), Box::new(|_| HttpResponse::ok(b"bbb"))),
        ],
        4,
        None,
    );
    let dir = tempfile_dir();
    let list = dir.join("urls.txt");
    std::fs::write(&list, format!("http://{addr}/a.bin\nhttp://{addr}/b.bin\n")).unwrap();
    let code = run_fetchling([
        "--no-config",
        "-q",
        "--tries=1",
        "--max-threads=2",
        "-P",
        dir.to_str().unwrap(),
        "-i",
        list.to_str().unwrap(),
    ])
    .await;
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(dir.join("a.bin")).unwrap(), b"aaa");
    assert_eq!(std::fs::read(dir.join("b.bin")).unwrap(), b"bbb");
    let _ = std::fs::remove_dir_all(&dir);
}
