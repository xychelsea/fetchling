//! Modular, non-interactive network retriever for the command line.
//!
//! `fetchling` downloads files and recursively mirrors remote content. It is
//! built on Tokio, Hyper, and rustls. Command-line parsing, protocol handling,
//! recursive retrieval, network primitives, and format processing live in
//! independent workspace crates. This crate is the executable: [`run`] /
//! [`run_with_args`] parse arguments, set up the process, and drive the engine.
//!
//! # What this crate is (and is not)
//!
//! IS: the `fetchling` command-line binary (a thin [`run`] / [`run_with_args`]
//! wrapper). [`run`] / [`run_with_args`] parse argv ([`parse_args`]), install a
//! rustls [`CryptoProvider`](rustls::crypto::CryptoProvider), optionally
//! daemonize on Unix (`-b`), start a Tokio runtime, and call [`Engine::run`].
//! The public API is flat at the crate root.
//!
//! IS NOT: an HTTP or FTP client, HTML/CSS/Metalink/WARC parsers, or a
//! re-export hub. This crate does not re-export [`Config`], [`Error`],
//! [`Engine`], or [`parse_args`]. For retrieval without argv, depend on
//! `fetchling-engine` (and `fetchling-core`); for argv only, `fetchling-cli`.
//!
//! # Typical integration
//!
//! Embed the CLI:
//!
//! 1. Call [`run`] (process argv) or [`run_with_args`]
//! 2. Map [`Result`] to a process status ([`Error::exit_code`] / [`ExitCode`])
//!
//! For retrieval without argv, depend on `fetchling-engine`: fill [`Config`],
//! [`Engine::new`], then [`Engine::run`]. For argv only, depend on
//! `fetchling-cli` ([`parse_args`]).
//!
//! # Areas
//!
//! - **Run** — [`run`], [`run_with_args`]
//!
//! # Examples
//!
//! Parse quiet download flags (no config file):
//!
//! ```
//! use fetchling_cli::{parse_args, ParseOutcome};
//!
//! let ParseOutcome::Run(cfg) =
//!     parse_args(["fetchling", "--no-config", "-q", "http://example.com"]).unwrap()
//! else {
//!     panic!("expected run");
//! };
//! assert!(cfg.quiet);
//! assert_eq!(cfg.urls, vec!["http://example.com"]);
//! ```
//!
//! Construct an engine from default config (no network):
//!
//! ```
//! use fetchling_core::Config;
//! use fetchling_engine::Engine;
//!
//! let mut cfg = Config::default();
//! cfg.quiet = true;
//! let engine = Engine::new(cfg).unwrap();
//! let _ = engine;
//! ```
//!
//! Run the CLI wiring (does not run; needs a server):
//!
//! ```no_run
//! use fetchling::run_with_args;
//!
//! let code = run_with_args(["--no-config", "-q", "https://example.com/file.bin"]).unwrap();
//! let _ = code;
//! ```

#![warn(missing_docs)]

use fetchling_cli::{parse_args, print_help, print_version, print_version_short, ParseOutcome};
use fetchling_core::{Config, ExitCode, Result};
use fetchling_engine::Engine;

#[cfg(doc)]
use fetchling_core::Error;

/// Parse process argv and run fetchling.
///
/// Installs the rustls ring [`CryptoProvider`](rustls::crypto::CryptoProvider),
/// then [`run_with_args`] on `std::env::args().skip(1)`.
///
/// # Errors
///
/// Returns [`Error`] from argv parsing, background setup, runtime creation, or
/// [`Engine::run`].
pub fn run() -> Result<ExitCode> {
    run_with_args(std::env::args().skip(1))
}

/// Parse `args` and run fetchling.
///
/// Installs the rustls ring [`CryptoProvider`](rustls::crypto::CryptoProvider).
/// [`ParseOutcome::Help`] / [`ParseOutcome::Version`] /
/// [`ParseOutcome::VersionShort`] print via `fetchling-cli` and return
/// [`ExitCode::Success`]. [`ParseOutcome::Run`] daemonizes when `-b` is set
/// (Unix), then builds a Tokio runtime and calls [`Engine::run`].
///
/// # Errors
///
/// Returns [`Error`] from [`parse_args`], background setup, runtime creation,
/// or [`Engine::run`].
pub fn run_with_args<I, S>(args: I) -> Result<ExitCode>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let _ =
        rustls::crypto::CryptoProvider::install_default(rustls::crypto::ring::default_provider());
    match parse_args(args)? {
        ParseOutcome::Help => {
            print_help();
            Ok(ExitCode::Success)
        }
        ParseOutcome::Version => {
            print_version();
            Ok(ExitCode::Success)
        }
        ParseOutcome::VersionShort => {
            print_version_short();
            Ok(ExitCode::Success)
        }
        ParseOutcome::Run(mut cfg) => {
            prepare_background(&mut cfg)?;
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    fetchling_core::Error::Io(std::io::Error::other(format!("tokio runtime: {e}")))
                })?;
            rt.block_on(async move { Engine::new(*cfg)?.run().await })
        }
    }
}

fn prepare_background(cfg: &mut Config) -> Result<()> {
    if !cfg.background {
        return Ok(());
    }
    #[cfg(unix)]
    {
        if cfg.logfile.is_none() {
            cfg.logfile = Some("fetchling-log".into());
        }
        let log_path = cfg.logfile.clone().expect("logfile set");
        let abs = absolutize_path(&log_path);
        cfg.logfile = Some(abs.clone());
        daemonize(&abs)?;
        cfg.background = false;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        eprintln!("fetchling: warning: background/daemonize is not supported on this platform; continuing in the foreground");
        cfg.background = false;
        Ok(())
    }
}

fn absolutize_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p).display().to_string(),
        Err(_) => path.to_string(),
    }
}

#[cfg(unix)]
fn daemonize(log_path: &str) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::process;

    use nix::unistd::{dup2, fork, setsid, ForkResult};

    match unsafe { fork() }
        .map_err(|e| fetchling_core::Error::Io(std::io::Error::other(format!("fork: {e}"))))?
    {
        ForkResult::Parent { .. } => process::exit(0),
        ForkResult::Child => {}
    }

    setsid()
        .map_err(|e| fetchling_core::Error::Io(std::io::Error::other(format!("setsid: {e}"))))?;

    match unsafe { fork() }
        .map_err(|e| fetchling_core::Error::Io(std::io::Error::other(format!("fork: {e}"))))?
    {
        ForkResult::Parent { .. } => process::exit(0),
        ForkResult::Child => {}
    }

    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| {
            fetchling_core::Error::Io(std::io::Error::new(
                e.kind(),
                format!("cannot open logfile {log_path}: {e}"),
            ))
        })?;
    let null = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|e| {
            fetchling_core::Error::Io(std::io::Error::new(
                e.kind(),
                format!("cannot open /dev/null: {e}"),
            ))
        })?;

    let log_fd = log.as_raw_fd();
    let null_fd = null.as_raw_fd();
    dup2(null_fd, 0).map_err(|e| {
        fetchling_core::Error::Io(std::io::Error::other(format!("dup2 stdin: {e}")))
    })?;
    dup2(log_fd, 1).map_err(|e| {
        fetchling_core::Error::Io(std::io::Error::other(format!("dup2 stdout: {e}")))
    })?;
    dup2(log_fd, 2).map_err(|e| {
        fetchling_core::Error::Io(std::io::Error::other(format!("dup2 stderr: {e}")))
    })?;

    let mut announce = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| {
            fetchling_core::Error::Io(std::io::Error::new(
                e.kind(),
                format!("cannot open logfile {log_path}: {e}"),
            ))
        })?;
    let _ = writeln!(announce, "fetchling: continuing in background");
    std::mem::forget(log);
    std::mem::forget(null);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchling_core::Error;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn run_nc(args: &[&str]) -> Result<ExitCode> {
        let mut v = vec!["--no-config"];
        v.extend(args.iter().copied());
        run_with_args(v)
    }

    #[test]
    fn absolutize_path_absolute_and_relative() {
        assert_eq!(absolutize_path("/tmp/x"), "/tmp/x");
        let p = absolutize_path("rel.bin");
        assert!(p.ends_with("rel.bin"));
        assert!(std::path::Path::new(&p).is_absolute());
    }

    #[test]
    fn prepare_background_noop_when_unset() {
        let mut cfg = Config::default();
        assert!(!cfg.background);
        assert!(cfg.logfile.is_none());
        prepare_background(&mut cfg).unwrap();
        assert!(!cfg.background);
        assert!(cfg.logfile.is_none());
    }

    #[test]
    fn run_with_args_rejects_unknown_deferred_missing_and_empty() {
        assert!(matches!(
            run_nc(&["--not-a-flag", "http://x"]).unwrap_err(),
            Error::InvalidOption(_)
        ));
        assert!(matches!(
            run_nc(&["--http2"]).unwrap_err(),
            Error::DeferredOption(_)
        ));
        assert!(matches!(
            run_nc(&["--output-document"]).unwrap_err(),
            Error::InvalidOption(_)
        ));
        let err = run_nc(&[]).unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
        assert!(err.to_string().contains("no URL specified"));
    }

    #[test]
    fn run_with_args_downloads_localhost_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = b"hello-run";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
        });

        let dir = std::env::temp_dir().join(format!(
            "fetchling-lib-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("out.bin");
        let dest_s = dest.to_str().unwrap();
        let url = format!("http://{addr}/file.bin");
        let code = run_nc(&["-q", "--tries=1", "-O", dest_s, &url]).unwrap();
        assert_eq!(code, ExitCode::Success);
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello-run");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
