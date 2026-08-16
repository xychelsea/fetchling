use std::net::SocketAddr;
use std::path::PathBuf;

use fetchling_core::{Config, ExitCode};
use fetchling_engine::{local_path_for_url, Engine};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

fn quiet_cfg() -> Config {
    Config {
        quiet: true,
        netrc: false,
        http_keep_alive: false,
        tries: 1,
        ..Config::default()
    }
}

fn temp_dir(kind: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("fetchling-engine-it-{kind}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn spawn_http(replies: Vec<(u16, Vec<u8>)>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for (status, body) in replies {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = match stream.read(&mut tmp).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let mut out = format!(
                "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            out.extend_from_slice(&body);
            let _ = stream.write_all(&out).await;
        }
    });
    addr
}

#[tokio::test]
async fn recursive_html_fetches_child() {
    let addr = spawn_http(vec![
        (200, b"User-agent: *\nDisallow:\n".to_vec()),
        (200, b"<html><a href=\"child.bin\">x</a></html>".to_vec()),
        (200, b"child".to_vec()),
    ])
    .await;
    let dir = temp_dir("recurse");
    let mut cfg = quiet_cfg();
    cfg.recursive = true;
    cfg.level = 1;
    cfg.directory_prefix = dir.display().to_string();
    cfg.urls = vec![format!("http://{addr}/index.html")];
    let code = Engine::new(cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    let path_cfg = Config {
        recursive: true,
        directory_prefix: dir.display().to_string(),
        ..Config::default()
    };
    let index = local_path_for_url(
        &path_cfg,
        &Url::parse(&format!("http://{addr}/index.html")).unwrap(),
    );
    let child = local_path_for_url(
        &path_cfg,
        &Url::parse(&format!("http://{addr}/child.bin")).unwrap(),
    );
    assert!(index.exists());
    assert_eq!(std::fs::read(&child).unwrap(), b"child");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn input_file_seeds_urls() {
    let addr = spawn_http(vec![(200, b"from-input".to_vec())]).await;
    let dir = temp_dir("input");
    let list = dir.join("urls.txt");
    std::fs::write(&list, format!("# skip\nhttp://{addr}/file.bin\n")).unwrap();
    let mut cfg = quiet_cfg();
    cfg.directory_prefix = dir.display().to_string();
    cfg.input_file = Some(list.display().to_string());
    let code = Engine::new(cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(dir.join("file.bin")).unwrap(), b"from-input");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn no_clobber_leaves_existing_file() {
    let dir = temp_dir("noclobber");
    let mut cfg = quiet_cfg();
    cfg.directory_prefix = dir.display().to_string();
    cfg.no_clobber = true;
    cfg.urls = vec!["http://127.0.0.1:1/file.bin".into()];
    let dest = local_path_for_url(&cfg, &Url::parse("http://127.0.0.1:1/file.bin").unwrap());
    std::fs::write(&dest, b"keep-me").unwrap();
    let code = Engine::new(cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep-me");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn quota_stops_after_first_file() {
    let addr = spawn_http(vec![(200, b"hello".to_vec()), (200, b"second".to_vec())]).await;
    let dir = temp_dir("quota");
    let mut cfg = quiet_cfg();
    cfg.directory_prefix = dir.display().to_string();
    cfg.quota = Some(1);
    cfg.urls = vec![
        format!("http://{addr}/a.bin"),
        format!("http://{addr}/b.bin"),
    ];
    let code = Engine::new(cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    assert!(dir.join("a.bin").exists());
    assert!(!dir.join("b.bin").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn input_metalink_downloads_listed_url() {
    let addr = spawn_http(vec![(200, b"metalink-body".to_vec())]).await;
    let dir = temp_dir("metalink");
    let meta = dir.join("f.meta4");
    std::fs::write(
        &meta,
        format!(
            r#"<?xml version="1.0"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="f.bin">
    <url>http://{addr}/f.bin</url>
  </file>
</metalink>"#
        ),
    )
    .unwrap();
    let mut cfg = quiet_cfg();
    cfg.directory_prefix = dir.display().to_string();
    cfg.input_metalink = Some(meta.display().to_string());
    let code = Engine::new(cfg).unwrap().run().await.unwrap();
    assert_eq!(code, ExitCode::Success);
    assert_eq!(std::fs::read(dir.join("f.bin")).unwrap(), b"metalink-body");
    let _ = std::fs::remove_dir_all(&dir);
}
