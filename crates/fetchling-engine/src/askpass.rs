//! Password prompting for `--ask-password` / `--use-askpass`.

use std::io::{BufRead, Write};
use std::process::Command;

use fetchling_core::{Config, Error, Result};

/// Fill `cfg.password` when prompting was requested and no password is set yet.
pub fn maybe_prompt_password(cfg: &mut Config) -> Result<()> {
    let want = cfg.ask_password || cfg.use_askpass.is_some();
    if !want {
        return Ok(());
    }
    if cfg.password.is_some() || cfg.http_password.is_some() || cfg.ftp_password.is_some() {
        return Ok(());
    }

    let prompt = "Password: ";
    let pass = if let Some(cmd) = cfg.use_askpass.clone() {
        run_askpass(&cmd, prompt)?
    } else {
        read_password_tty(prompt)?
    };
    cfg.password = Some(pass);
    Ok(())
}

fn run_askpass(cmd: &str, prompt: &str) -> Result<String> {
    let cmd = if cmd.is_empty() {
        std::env::var("SSH_ASKPASS")
            .map_err(|_| Error::Auth("--use-askpass requires a command or SSH_ASKPASS".into()))?
    } else {
        cmd.to_string()
    };
    let output = Command::new(&cmd)
        .arg(prompt)
        .output()
        .map_err(|e| Error::Auth(format!("askpass `{cmd}` failed: {e}")))?;
    if !output.status.success() {
        return Err(Error::Auth(format!(
            "askpass `{cmd}` exited with {}",
            output.status
        )));
    }
    let line = String::from_utf8_lossy(&output.stdout);
    Ok(line.lines().next().unwrap_or("").to_string())
}

fn read_password_tty(prompt: &str) -> Result<String> {
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok();

    if let Some(ref mut t) = tty {
        let _ = t.write_all(prompt.as_bytes());
        let _ = t.flush();
    } else {
        let mut err = std::io::stderr();
        let _ = err.write_all(prompt.as_bytes());
        let _ = err.flush();
    }

    #[cfg(unix)]
    let echo_guard = tty.as_ref().and_then(EchoOffGuard::new);

    let line = if let Some(ref t) = tty {
        let mut reader = std::io::BufReader::new(t);
        let mut s = String::new();
        reader
            .read_line(&mut s)
            .map_err(|e| Error::Auth(format!("read password: {e}")))?;
        s
    } else {
        let mut s = String::new();
        std::io::stdin()
            .read_line(&mut s)
            .map_err(|e| Error::Auth(format!("read password: {e}")))?;
        s
    };

    #[cfg(unix)]
    drop(echo_guard);

    #[cfg(unix)]
    {
        let _ = writeln!(std::io::stderr());
    }

    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

#[cfg(unix)]
struct EchoOffGuard {
    fd: std::os::fd::RawFd,
    original: nix::sys::termios::Termios,
}

#[cfg(unix)]
impl EchoOffGuard {
    fn new(tty: &std::fs::File) -> Option<Self> {
        use nix::sys::termios::{self, LocalFlags, SetArg};
        use std::os::fd::AsRawFd;

        let original = termios::tcgetattr(tty).ok()?;
        let mut noecho = original.clone();
        noecho.local_flags.remove(LocalFlags::ECHO);
        termios::tcsetattr(tty, SetArg::TCSANOW, &noecho).ok()?;
        Some(Self {
            fd: tty.as_raw_fd(),
            original,
        })
    }
}

#[cfg(unix)]
impl Drop for EchoOffGuard {
    fn drop(&mut self) {
        use nix::sys::termios::{self, SetArg};
        use std::os::fd::BorrowedFd;

        let fd = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = termios::tcsetattr(fd, SetArg::TCSANOW, &self.original);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_askpass_echo(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("askpass");
        std::fs::write(&path, b"#!/usr/bin/env sh\nprintf '%s\\n' \"$1\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    #[test]
    fn askpass_printf_fills_password() {
        let dir = std::env::temp_dir().join(format!("fetchling-askpass-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let askpass = make_askpass_echo(&dir);
        let mut cfg = Config {
            use_askpass: Some(askpass.display().to_string()),
            ..Config::default()
        };
        maybe_prompt_password(&mut cfg).unwrap();
        assert_eq!(cfg.password.as_deref(), Some("Password: "));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_when_password_already_set() {
        let mut cfg = Config {
            ask_password: true,
            password: Some("secret".into()),
            ..Config::default()
        };
        maybe_prompt_password(&mut cfg).unwrap();
        assert_eq!(cfg.password.as_deref(), Some("secret"));
    }

    #[test]
    fn skips_when_not_requested() {
        let mut cfg = Config::default();
        maybe_prompt_password(&mut cfg).unwrap();
        assert!(cfg.password.is_none());
    }
}
