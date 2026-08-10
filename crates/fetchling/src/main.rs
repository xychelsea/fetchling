use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::process;

use fetchling_cli::{parse_args, print_help, print_version, print_version_short, ParseOutcome};
use fetchling_core::{Config, ExitCode, Result};
use fetchling_engine::Engine;

fn main() {
    let _ =
        rustls::crypto::CryptoProvider::install_default(rustls::crypto::ring::default_provider());
    let code = match run() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fetchling: {e}");
            e.exit_code()
        }
    };
    process::exit(i32::from(code));
}

fn run() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
        daemonize(&log_path)?;
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

#[cfg(unix)]
fn daemonize(log_path: &str) -> Result<()> {
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
