use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use fetchling_core::{dest_label, match_glob, Config, Error, ProgressBar, Result};
use fetchling_net::{
    build_connector_resumable, connect_happy_eyeballs, connect_tcp, read_timeout_dur, DnsCache,
};
use rustls::pki_types::ServerName;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FtpEntryKind {
    File,
    Dir,
    Symlink { target: Option<String> },
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtpEntry {
    pub name: String,
    pub kind: FtpEntryKind,
}

#[derive(Debug, Clone)]
pub enum FtpDownloadOutcome {
    File { bytes: u64 },
    Listing { bytes: u64, entries: Vec<FtpEntry> },
}

pub struct FtpClient {
    pub dns: DnsCache,
}

impl Default for FtpClient {
    fn default() -> Self {
        Self {
            dns: DnsCache::new(),
        }
    }
}

enum FtpConn {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl FtpConn {
    fn peer_addr(&self) -> std::io::Result<SocketAddr> {
        match self {
            Self::Plain(s) => s.peer_addr(),
            Self::Tls(s) => s.get_ref().0.peer_addr(),
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        match self {
            Self::Plain(s) => s.local_addr(),
            Self::Tls(s) => s.get_ref().0.local_addr(),
        }
    }
}

impl AsyncRead for FtpConn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for FtpConn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut *self {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

struct FtpsTls {
    data_connector: TlsConnector,
    server_name: ServerName<'static>,
}

struct FtpSession {
    control: FtpConn,
    data_tls: bool,
    ftps: Option<FtpsTls>,
    read_to: Option<Duration>,
}

fn wants_ftps(url: &Url) -> bool {
    url.scheme() == "ftps"
}

fn ftp_control_port(url: &Url, cfg: &Config) -> u16 {
    match url.port() {
        Some(21) if wants_ftps(url) && cfg.ftps_implicit => 990,
        Some(p) => p,
        None if wants_ftps(url) && cfg.ftps_implicit => 990,
        None => 21,
    }
}

fn ftps_data_tls(control_tls: bool, cfg: &Config) -> bool {
    control_tls && !cfg.ftps_clear_data_connection
}

fn auth_tls_accepted(reply: &str) -> bool {
    reply.starts_with('2') || reply.starts_with('3')
}

impl FtpClient {
    pub async fn download(
        &self,
        cfg: &Config,
        url: &Url,
        dest: &Path,
    ) -> Result<FtpDownloadOutcome> {
        if url.scheme() != "ftp" && url.scheme() != "ftps" {
            return Err(Error::Protocol(format!("not an FTP URL: {}", url.scheme())));
        }
        let mut session = self.open_session(cfg, url).await?;
        let path = url.path();
        let path = if path.is_empty() { "/" } else { path };
        reject_ftp_injection(path, "FTP path")?;

        let is_dir = path.ends_with('/');
        if is_dir {
            return list_directory(cfg, &mut session, path, dest).await;
        }

        send_cmd(&mut session.control, "TYPE I").await?;
        let _ = read_reply(&mut session.control, session.read_to).await?;

        let control_ip = session
            .control
            .peer_addr()
            .map_err(|e| Error::Network(format!("FTP peer addr: {e}")))?
            .ip();

        let mode_bits = if cfg.preserve_permissions {
            let mut mode = query_unix_mode(&mut session.control, path, session.read_to).await;
            if mode.is_none() {
                mode = query_unix_mode_from_list(cfg, &mut session, path, control_ip).await;
            }
            mode
        } else {
            None
        };

        let data_channel =
            open_data_channel(cfg, &mut session.control, control_ip, session.read_to).await?;

        let mut start_pos = cfg.start_pos.unwrap_or(0);
        if start_pos == 0 && cfg.continue_download && dest.exists() {
            start_pos = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        }

        if start_pos > 0 {
            send_cmd(&mut session.control, &format!("SIZE {path}")).await?;
            let size_reply = read_reply(&mut session.control, session.read_to).await?;
            if size_reply.starts_with('2') {
                if let Some(total) = size_reply
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    if start_pos >= total {
                        return Ok(FtpDownloadOutcome::File { bytes: 0 });
                    }
                }
            }
            send_cmd(&mut session.control, &format!("REST {start_pos}")).await?;
            let rest = read_reply(&mut session.control, session.read_to).await?;
            if !rest.starts_with('3') {
                return Err(Error::Protocol(format!("REST failed: {rest}")));
            }
        }

        send_cmd(&mut session.control, &format!("RETR {path}")).await?;

        let mut data = accept_data_conn(cfg, data_channel, &session, control_ip).await?;
        let retr = read_reply(&mut session.control, session.read_to).await?;
        if !(retr.starts_with('1') || retr.starts_with('2')) {
            return Err(Error::Server(format!("RETR failed: {retr}")));
        }
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut file = if start_pos > 0 {
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dest)
                .await?
        } else {
            tokio::fs::File::create(dest).await?
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut written = 0u64;
        let mut progress = ProgressBar::with_initial(cfg, None, start_pos, dest_label(dest));
        loop {
            let n = timed_read(session.read_to, data.read(&mut buf)).await?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).await?;
            written += n as u64;
            progress.update(n as u64);
        }
        progress.finish();
        drop(data);

        let _ = read_reply(&mut session.control, session.read_to).await;
        let _ = send_cmd(&mut session.control, "QUIT").await;

        if let Some(mode) = mode_bits {
            apply_unix_mode(dest, mode);
        } else if cfg.preserve_permissions {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!(
                    "fetchling: warning: --preserve-permissions: could not determine mode via MLST or LIST"
                );
            });
        }

        Ok(FtpDownloadOutcome::File { bytes: written })
    }

    pub async fn expand_glob(&self, cfg: &Config, url: &Url) -> Result<Vec<String>> {
        if url.scheme() != "ftp" && url.scheme() != "ftps" {
            return Err(Error::Protocol(format!("not an FTP URL: {}", url.scheme())));
        }
        let mut session = self.open_session(cfg, url).await?;

        let path = if url.path().is_empty() {
            "/"
        } else {
            url.path()
        };
        let pattern = path.rsplit('/').next().unwrap_or(path);
        let parent = if let Some(i) = path.rfind('/') {
            if i == 0 {
                "/"
            } else {
                &path[..=i]
            }
        } else {
            "/"
        };
        reject_ftp_injection(parent, "FTP path")?;

        send_cmd(&mut session.control, "TYPE A").await?;
        let _ = read_reply(&mut session.control, session.read_to).await?;
        let control_ip = session
            .control
            .peer_addr()
            .map_err(|e| Error::Network(format!("FTP peer addr: {e}")))?
            .ip();

        let listing = {
            let data_channel =
                open_data_channel(cfg, &mut session.control, control_ip, session.read_to).await?;
            let list_path = if parent == "/" {
                "/"
            } else {
                parent.trim_end_matches('/')
            };
            send_cmd(&mut session.control, &format!("NLST {list_path}")).await?;
            let reply = read_reply(&mut session.control, session.read_to).await?;
            let mut data = if reply.starts_with('1') || reply.starts_with('2') {
                accept_data_conn(cfg, data_channel, &session, control_ip).await?
            } else {
                drop(data_channel);
                let data_channel =
                    open_data_channel(cfg, &mut session.control, control_ip, session.read_to)
                        .await?;
                send_cmd(&mut session.control, &format!("LIST {list_path}")).await?;
                let list_reply = read_reply(&mut session.control, session.read_to).await?;
                if !(list_reply.starts_with('1') || list_reply.starts_with('2')) {
                    return Err(Error::Server(format!("NLST/LIST failed for glob: {reply}")));
                }
                accept_data_conn(cfg, data_channel, &session, control_ip).await?
            };
            let mut buf = Vec::new();
            data.read_to_end(&mut buf).await?;
            drop(data);
            let _ = read_reply(&mut session.control, session.read_to).await;
            let _ = send_cmd(&mut session.control, "QUIT").await;
            String::from_utf8_lossy(&buf).into_owned()
        };

        let mut names = Vec::new();
        for line in listing.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let base = line.rsplit([' ', '\t', '/']).next().unwrap_or(line);
            if base == "." || base == ".." {
                continue;
            }
            if match_glob(base, pattern, cfg.ignore_case) {
                names.push(base.to_string());
            }
        }
        names.sort();
        names.dedup();
        if names.is_empty() {
            return Err(Error::Server(format!(
                "FTP glob '{pattern}' matched no files"
            )));
        }
        Ok(names)
    }

    async fn open_session(&self, cfg: &Config, url: &Url) -> Result<FtpSession> {
        let host = url
            .host_str()
            .ok_or_else(|| Error::Parse("FTP URL missing host".into()))?;
        let port = ftp_control_port(url, cfg);
        let addrs = self.dns.lookup(cfg, host, port).await?;
        let tcp = connect_happy_eyeballs(cfg, &addrs).await?;
        let read_to = read_timeout_dur(cfg);
        let ftps = wants_ftps(url);

        let (mut control, control_tls, ftps_ctx) = if !ftps {
            let mut control = FtpConn::Plain(tcp);
            let welcome = read_reply(&mut control, read_to).await?;
            if !welcome.starts_with('2') {
                return Err(Error::Protocol(format!("FTP welcome: {welcome}")));
            }
            (control, false, None)
        } else {
            let server_name = ServerName::try_from(host.to_string())
                .map_err(|e| Error::Tls(format!("server name: {e}")))?;
            let control_connector = build_connector_resumable(cfg, true)?;
            let data_connector = if cfg.ftps_resume_ssl {
                control_connector.clone()
            } else {
                build_connector_resumable(cfg, false)?
            };
            let ftps_ctx = FtpsTls {
                data_connector,
                server_name: server_name.clone(),
            };

            if cfg.ftps_implicit {
                let tls = control_connector
                    .connect(server_name, tcp)
                    .await
                    .map_err(|e| Error::Tls(format!("FTPS handshake: {e}")))?;
                let mut control = FtpConn::Tls(Box::new(tls));
                let welcome = read_reply(&mut control, read_to).await?;
                if !welcome.starts_with('2') {
                    return Err(Error::Protocol(format!("FTP welcome: {welcome}")));
                }
                (control, true, Some(ftps_ctx))
            } else {
                let mut control = FtpConn::Plain(tcp);
                let welcome = read_reply(&mut control, read_to).await?;
                if !welcome.starts_with('2') {
                    return Err(Error::Protocol(format!("FTP welcome: {welcome}")));
                }
                send_cmd(&mut control, "AUTH TLS").await?;
                let auth = read_reply(&mut control, read_to).await?;
                if auth_tls_accepted(&auth) {
                    let tcp = match control {
                        FtpConn::Plain(s) => s,
                        FtpConn::Tls(_) => {
                            return Err(Error::Tls("unexpected TLS control before AUTH".into()));
                        }
                    };
                    let tls = control_connector
                        .connect(server_name, tcp)
                        .await
                        .map_err(|e| Error::Tls(format!("FTPS handshake: {e}")))?;
                    (FtpConn::Tls(Box::new(tls)), true, Some(ftps_ctx))
                } else if cfg.ftps_fallback_to_ftp {
                    (control, false, None)
                } else {
                    return Err(Error::Tls(format!("AUTH TLS failed: {auth}")));
                }
            }
        };

        if control_tls {
            send_cmd(&mut control, "PBSZ 0").await?;
            let pbsz = read_reply(&mut control, read_to).await?;
            if !pbsz.starts_with('2') {
                return Err(Error::Protocol(format!("PBSZ failed: {pbsz}")));
            }
            let prot = if cfg.ftps_clear_data_connection {
                "PROT C"
            } else {
                "PROT P"
            };
            send_cmd(&mut control, prot).await?;
            let prot_reply = read_reply(&mut control, read_to).await?;
            if !prot_reply.starts_with('2') {
                return Err(Error::Protocol(format!("{prot} failed: {prot_reply}")));
            }
        }

        ftp_login(&mut control, cfg, url, host, read_to).await?;

        Ok(FtpSession {
            control,
            data_tls: ftps_data_tls(control_tls, cfg),
            ftps: if control_tls { ftps_ctx } else { None },
            read_to,
        })
    }
}

async fn ftp_login(
    control: &mut FtpConn,
    cfg: &Config,
    url: &Url,
    host: &str,
    read_to: Option<Duration>,
) -> Result<()> {
    let mut user = cfg
        .ftp_user
        .as_ref()
        .or(cfg.user.as_ref())
        .map(|s| s.to_string())
        .or_else(|| {
            if url.username().is_empty() {
                None
            } else {
                Some(url.username().to_string())
            }
        });
    let mut pass = cfg
        .ftp_password
        .as_ref()
        .or(cfg.password.as_ref())
        .map(|s| s.to_string())
        .or_else(|| url.password().map(|p| p.to_string()));

    if cfg.netrc && (user.is_none() || pass.is_none()) {
        if let Some((nuser, npass)) =
            fetchling_core::lookup_credentials(host, cfg.netrc_file.as_deref())
        {
            if user.is_none() {
                user = Some(nuser);
            }
            if pass.is_none() {
                pass = Some(npass);
            }
        }
    }

    let user = user.as_deref().unwrap_or("anonymous");
    let pass = pass.as_deref().unwrap_or("fetchling@");

    reject_ftp_injection(user, "FTP user")?;
    reject_ftp_injection(pass, "FTP password")?;

    send_cmd(control, &format!("USER {user}")).await?;
    let r = read_reply(control, read_to).await?;
    if r.starts_with('3') {
        send_cmd(control, &format!("PASS {pass}")).await?;
        let r = read_reply(control, read_to).await?;
        if !r.starts_with('2') {
            return Err(Error::Auth(format!("FTP login failed: {r}")));
        }
    } else if !r.starts_with('2') {
        return Err(Error::Auth(format!("FTP USER failed: {r}")));
    }
    Ok(())
}

async fn list_directory(
    cfg: &Config,
    session: &mut FtpSession,
    path: &str,
    dest: &Path,
) -> Result<FtpDownloadOutcome> {
    send_cmd(&mut session.control, "TYPE A").await?;
    let _ = read_reply(&mut session.control, session.read_to).await?;

    let control_ip = session
        .control
        .peer_addr()
        .map_err(|e| Error::Network(format!("FTP peer addr: {e}")))?
        .ip();

    let list_path = if path == "/" {
        path
    } else {
        path.trim_end_matches('/')
    };

    let mut used_mlsd = false;
    let mut used_nlst_only = false;
    let mut data = {
        let data_channel =
            open_data_channel(cfg, &mut session.control, control_ip, session.read_to).await?;
        send_cmd(&mut session.control, &format!("MLSD {list_path}")).await?;
        let mlsd_reply = read_reply(&mut session.control, session.read_to).await?;
        if mlsd_reply.starts_with('1') || mlsd_reply.starts_with('2') {
            used_mlsd = true;
            accept_data_conn(cfg, data_channel, session, control_ip).await?
        } else {
            drop(data_channel);
            let data_channel =
                open_data_channel(cfg, &mut session.control, control_ip, session.read_to).await?;
            send_cmd(&mut session.control, &format!("LIST {list_path}")).await?;
            let list_reply = read_reply(&mut session.control, session.read_to).await?;
            if list_reply.starts_with('1') || list_reply.starts_with('2') {
                accept_data_conn(cfg, data_channel, session, control_ip).await?
            } else {
                drop(data_channel);
                let data_channel =
                    open_data_channel(cfg, &mut session.control, control_ip, session.read_to)
                        .await?;
                send_cmd(&mut session.control, &format!("NLST {list_path}")).await?;
                let nlst = read_reply(&mut session.control, session.read_to).await?;
                if !(nlst.starts_with('1') || nlst.starts_with('2')) {
                    return Err(Error::Server(format!(
                        "MLSD/LIST/NLST failed: {list_reply}"
                    )));
                }
                used_nlst_only = true;
                accept_data_conn(cfg, data_channel, session, control_ip).await?
            }
        }
    };

    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = tokio::fs::File::create(dest).await?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut raw = Vec::new();
    let mut written = 0u64;
    let mut progress = ProgressBar::with_initial(cfg, None, 0, dest_label(dest));
    loop {
        let n = timed_read(session.read_to, data.read(&mut buf)).await?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).await?;
        raw.extend_from_slice(&buf[..n]);
        written += n as u64;
        progress.update(n as u64);
    }
    progress.finish();
    drop(data);
    let _ = read_reply(&mut session.control, session.read_to).await;
    let _ = send_cmd(&mut session.control, "QUIT").await;

    let text = String::from_utf8_lossy(&raw);
    let entries = if used_nlst_only {
        Vec::new()
    } else if used_mlsd {
        parse_mlsd(&text)
    } else {
        parse_unix_list(&text)
    };

    if cfg.remove_listing {
        let _ = std::fs::remove_file(dest);
    }
    Ok(FtpDownloadOutcome::Listing {
        bytes: written,
        entries,
    })
}

fn sanitize_entry_name(name: &str) -> Option<String> {
    let name = name.trim().trim_matches('"');
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    Some(name.to_string())
}

pub fn parse_mlsd(text: &str) -> Vec<FtpEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (facts, name) = match line.rsplit_once(' ') {
            Some((f, n)) => (f, n),
            None => continue,
        };
        let Some(name) = sanitize_entry_name(name) else {
            continue;
        };
        let mut kind = FtpEntryKind::Other;
        let mut symlink_target = None;
        for fact in facts.split(';') {
            let fact = fact.trim();
            if fact.is_empty() {
                continue;
            }
            let (k, v) = match fact.split_once('=') {
                Some(pair) => pair,
                None => continue,
            };
            let k = k.to_ascii_lowercase();
            if k == "type" {
                let v = v.to_ascii_lowercase();
                kind = if v == "dir" || v == "cdir" || v == "pdir" {
                    FtpEntryKind::Dir
                } else if v == "file" {
                    FtpEntryKind::File
                } else if v.starts_with("os.unix=symlink") || v.contains("symlink") {
                    FtpEntryKind::Symlink { target: None }
                } else {
                    FtpEntryKind::Other
                };
            } else if k == "unix.slink" || k == "unix.symlink" {
                symlink_target = Some(v.to_string());
            }
        }
        if let FtpEntryKind::Symlink { .. } = kind {
            kind = FtpEntryKind::Symlink {
                target: symlink_target,
            };
        } else if let Some(t) = symlink_target {
            kind = FtpEntryKind::Symlink { target: Some(t) };
        }
        out.push(FtpEntry { name, kind });
    }
    out
}

pub fn parse_unix_list(text: &str) -> Vec<FtpEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let bytes = line.as_bytes();
        if bytes.is_empty() {
            continue;
        }
        let kind_ch = bytes[0] as char;
        let kind = match kind_ch {
            'd' => FtpEntryKind::Dir,
            '-' => FtpEntryKind::File,
            'l' => FtpEntryKind::Symlink { target: None },
            _ => continue,
        };
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let name_part = parts[8..].join(" ");
        let (name, target) = if matches!(kind, FtpEntryKind::Symlink { .. }) {
            if let Some((n, t)) = name_part.split_once(" -> ") {
                (n.trim(), Some(t.trim().to_string()))
            } else {
                (name_part.as_str(), None)
            }
        } else {
            (name_part.as_str(), None)
        };
        let Some(name) = sanitize_entry_name(name) else {
            continue;
        };
        let kind = match kind {
            FtpEntryKind::Symlink { .. } => FtpEntryKind::Symlink { target },
            other => other,
        };
        out.push(FtpEntry { name, kind });
    }
    out
}

enum DataChannel {
    Passive(SocketAddr),
    Active(tokio::net::TcpListener),
}

async fn open_data_channel(
    cfg: &Config,
    stream: &mut FtpConn,
    control_ip: IpAddr,
    read_to: Option<Duration>,
) -> Result<DataChannel> {
    if cfg.passive_ftp {
        send_cmd(stream, "PASV").await?;
        let pasv = read_reply(stream, read_to).await?;
        let addr = parse_pasv(&pasv)?;
        validate_pasv_addr(addr, control_ip)?;
        Ok(DataChannel::Passive(addr))
    } else {
        let (listener, port_cmd) = prepare_active_listener(cfg, stream).await?;
        send_cmd(stream, &port_cmd).await?;
        let r = read_reply(stream, read_to).await?;
        if !r.starts_with('2') {
            return Err(Error::Protocol(format!("PORT/EPRT failed: {r}")));
        }
        Ok(DataChannel::Active(listener))
    }
}

async fn accept_data(
    cfg: &Config,
    data_channel: DataChannel,
    control_ip: IpAddr,
) -> Result<TcpStream> {
    match data_channel {
        DataChannel::Passive(addr) => connect_tcp(cfg, addr).await,
        DataChannel::Active(listener) => {
            let (tcp, peer) = timeout(Duration::from_secs(60), listener.accept())
                .await
                .map_err(|_| Error::Network("active FTP data accept timeout".into()))?
                .map_err(|e| Error::Network(format!("active FTP accept: {e}")))?;
            validate_pasv_addr(peer, control_ip)?;
            Ok(tcp)
        }
    }
}

async fn accept_data_conn(
    cfg: &Config,
    data_channel: DataChannel,
    session: &FtpSession,
    control_ip: IpAddr,
) -> Result<FtpConn> {
    let tcp = accept_data(cfg, data_channel, control_ip).await?;
    if session.data_tls {
        let ftps = session
            .ftps
            .as_ref()
            .ok_or_else(|| Error::Tls("FTPS data TLS missing context".into()))?;
        let tls = ftps
            .data_connector
            .connect(ftps.server_name.clone(), tcp)
            .await
            .map_err(|e| Error::Tls(format!("FTPS data handshake: {e}")))?;
        Ok(FtpConn::Tls(Box::new(tls)))
    } else {
        Ok(FtpConn::Plain(tcp))
    }
}

async fn query_unix_mode(
    stream: &mut FtpConn,
    path: &str,
    read_to: Option<Duration>,
) -> Option<u32> {
    if send_cmd(stream, &format!("MLST {path}")).await.is_err() {
        return None;
    }
    let text = read_multiline_reply(stream, read_to).await.ok()?;
    if !text
        .lines()
        .next()
        .map(|l| l.starts_with('2'))
        .unwrap_or(false)
    {
        return None;
    }
    parse_unix_mode_from_mlst(&text)
}

async fn query_unix_mode_from_list(
    cfg: &Config,
    session: &mut FtpSession,
    path: &str,
    control_ip: IpAddr,
) -> Option<u32> {
    let basename = path.rsplit('/').next().filter(|s| !s.is_empty())?;
    let list_path = match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((parent, _)) if !parent.is_empty() => parent,
        _ => ".",
    };
    send_cmd(&mut session.control, "TYPE A").await.ok()?;
    let _ = read_reply(&mut session.control, session.read_to)
        .await
        .ok()?;
    let data_channel = open_data_channel(cfg, &mut session.control, control_ip, session.read_to)
        .await
        .ok()?;
    send_cmd(&mut session.control, &format!("LIST {list_path}"))
        .await
        .ok()?;
    let list_reply = read_reply(&mut session.control, session.read_to)
        .await
        .ok()?;
    if !(list_reply.starts_with('1') || list_reply.starts_with('2')) {
        return None;
    }
    let mut data = accept_data_conn(cfg, data_channel, session, control_ip)
        .await
        .ok()?;
    let mut raw = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = timed_read(session.read_to, data.read(&mut buf))
            .await
            .ok()?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    drop(data);
    let _ = read_reply(&mut session.control, session.read_to).await;
    let _ = send_cmd(&mut session.control, "TYPE I").await;
    let _ = read_reply(&mut session.control, session.read_to).await;
    let text = String::from_utf8_lossy(&raw);
    unix_mode_for_basename(&text, basename)
}

fn unix_mode_for_basename(list_text: &str, basename: &str) -> Option<u32> {
    for line in list_text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let name_part = parts[8..].join(" ");
        let name = if let Some((n, _)) = name_part.split_once(" -> ") {
            n.trim()
        } else {
            name_part.as_str()
        };
        let Some(name) = sanitize_entry_name(name) else {
            continue;
        };
        if name == basename {
            return parse_unix_mode_from_list_line(line);
        }
    }
    None
}

pub fn parse_unix_mode_from_list_line(line: &str) -> Option<u32> {
    let line = line.trim();
    let bytes = line.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    if !matches!(bytes[0], b'-' | b'd' | b'l') {
        return None;
    }
    let mut mode = 0u32;
    for (i, &b) in bytes[1..10].iter().enumerate() {
        let shift = (2 - i / 3) * 3;
        let bit = match b {
            b'r' => 4u32,
            b'w' => 2,
            b'x' | b's' | b't' => 1,
            b'-' | b'S' | b'T' => 0,
            _ => return None,
        };
        mode |= bit << shift;
        match (i, b) {
            (2, b's' | b'S') => mode |= 0o4000,
            (5, b's' | b'S') => mode |= 0o2000,
            (8, b't' | b'T') => mode |= 0o1000,
            _ => {}
        }
    }
    Some(mode)
}

async fn read_multiline_reply(stream: &mut FtpConn, read_to: Option<Duration>) -> Result<String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut out = String::new();
    let mut code: Option<[u8; 3]> = None;
    loop {
        line.clear();
        let n = timed_read(read_to, reader.read_line(&mut line)).await?;
        if n == 0 {
            return Err(Error::Network("FTP connection closed".into()));
        }
        let trimmed = line.trim_end();
        out.push_str(trimmed);
        out.push('\n');
        if trimmed.len() >= 4 && trimmed.as_bytes()[0].is_ascii_digit() {
            let c = [
                trimmed.as_bytes()[0],
                trimmed.as_bytes()[1],
                trimmed.as_bytes()[2],
            ];
            if code.is_none() {
                code = Some(c);
            }
            if trimmed.as_bytes()[3] == b' ' && code == Some(c) {
                return Ok(out);
            }
        }
    }
}

fn parse_unix_mode_from_mlst(text: &str) -> Option<u32> {
    for token in text.split(|c: char| c == ';' || c.is_whitespace()) {
        let token = token.trim();
        if let Some(val) = token
            .strip_prefix("unix.mode=")
            .or_else(|| token.strip_prefix("UNIX.mode="))
            .or_else(|| token.strip_prefix("Unix.mode="))
        {
            let digits: String = val.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                continue;
            }
            return u32::from_str_radix(&digits, 8).ok();
        }
    }
    None
}

fn apply_unix_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode & 0o777);
        let _ = std::fs::set_permissions(path, perms);
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

fn reject_ftp_injection(value: &str, label: &str) -> Result<()> {
    if value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return Err(Error::Protocol(format!(
            "{label} contains illegal control characters"
        )));
    }
    Ok(())
}

fn validate_pasv_addr(data: SocketAddr, control_ip: IpAddr) -> Result<()> {
    if data.ip() != control_ip {
        return Err(Error::Protocol(format!(
            "PASV address {} does not match control peer {}",
            data.ip(),
            control_ip
        )));
    }
    Ok(())
}

async fn prepare_active_listener(
    cfg: &Config,
    control: &FtpConn,
) -> Result<(tokio::net::TcpListener, String)> {
    let local = control
        .local_addr()
        .map_err(|e| Error::Network(format!("FTP local addr: {e}")))?;
    let bind_ip = if let Some(bind) = &cfg.bind_address {
        if let Ok(ip) = bind.parse::<IpAddr>() {
            if ip.is_ipv4() != local.is_ipv4() {
                return Err(Error::Network(format!(
                    "bind-address {bind} family does not match FTP control {local}"
                )));
            }
            ip
        } else {
            local.ip()
        }
    } else {
        local.ip()
    };
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(bind_ip, 0))
        .await
        .map_err(|e| Error::Network(format!("active FTP bind: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| Error::Network(format!("active FTP local addr: {e}")))?;
    Ok((listener, format_port_command(addr)))
}

fn format_port_command(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(v4) => {
            let o = v4.ip().octets();
            let p = v4.port();
            format!(
                "PORT {},{},{},{},{},{}",
                o[0],
                o[1],
                o[2],
                o[3],
                p / 256,
                p % 256
            )
        }
        SocketAddr::V6(v6) => format!("EPRT |2|{}|{}|", v6.ip(), v6.port()),
    }
}

async fn send_cmd(stream: &mut FtpConn, cmd: &str) -> Result<()> {
    stream.write_all(format!("{cmd}\r\n").as_bytes()).await?;
    Ok(())
}

async fn timed_read<T, E, F>(dur: Option<Duration>, fut: F) -> Result<T>
where
    E: Into<Error>,
    F: std::future::Future<Output = std::result::Result<T, E>>,
{
    if let Some(t) = dur {
        match timeout(t, fut).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Err(Error::Network("read timeout".into())),
        }
    } else {
        fut.await.map_err(Into::into)
    }
}

async fn read_reply(stream: &mut FtpConn, read_to: Option<Duration>) -> Result<String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        let n = timed_read(read_to, reader.read_line(&mut line)).await?;
        if n == 0 {
            return Err(Error::Network("FTP connection closed".into()));
        }
        let trimmed = line.trim_end().to_string();
        if trimmed.len() >= 4
            && trimmed.as_bytes()[0].is_ascii_digit()
            && trimmed.as_bytes()[3] == b' '
        {
            return Ok(trimmed);
        }
        if trimmed.len() >= 4
            && trimmed.as_bytes()[0].is_ascii_digit()
            && trimmed.as_bytes()[3] == b'-'
        {
            continue;
        }
    }
}

fn parse_pasv(reply: &str) -> Result<SocketAddr> {
    let start = reply
        .find('(')
        .ok_or_else(|| Error::Protocol(format!("bad PASV: {reply}")))?;
    let end = reply
        .find(')')
        .ok_or_else(|| Error::Protocol(format!("bad PASV: {reply}")))?;
    let inner = &reply[start + 1..end];
    let parts: Vec<_> = inner.split(',').collect();
    if parts.len() < 6 {
        return Err(Error::Protocol(format!("bad PASV parts: {reply}")));
    }
    let ip = format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[3]);
    let port: u16 =
        (parts[4].parse::<u16>().unwrap_or(0) * 256) + parts[5].parse::<u16>().unwrap_or(0);
    format!("{ip}:{port}")
        .parse()
        .map_err(|e| Error::Protocol(format!("PASV addr: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[test]
    fn parse_pasv_ok() {
        let addr = parse_pasv("227 Entering Passive Mode (127,0,0,1,4,1)").unwrap();
        assert_eq!(addr, SocketAddr::from(([127, 0, 0, 1], 1025)));
    }

    #[test]
    fn pasv_rejects_mismatched_ip() {
        let data = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 5000));
        let err = validate_pasv_addr(data, IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn pasv_accepts_same_ip() {
        let data = SocketAddr::from(([192, 0, 2, 1], 40000));
        validate_pasv_addr(data, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))).unwrap();
    }

    #[test]
    fn reject_crlf_in_user() {
        let err = reject_ftp_injection("evil\r\nQUIT", "FTP user").unwrap_err();
        assert!(err.to_string().contains("illegal control"));
    }

    #[test]
    fn reject_nul_in_path() {
        let err = reject_ftp_injection("a\0b", "FTP path").unwrap_err();
        assert!(err.to_string().contains("illegal control"));
    }

    #[test]
    fn format_port_ipv4() {
        let addr = SocketAddr::from(([192, 0, 2, 1], 1025));
        assert_eq!(format_port_command(addr), "PORT 192,0,2,1,4,1");
    }

    #[test]
    fn format_eprt_ipv6() {
        let addr = SocketAddr::from(([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1], 2121));
        let cmd = format_port_command(addr);
        assert!(cmd.starts_with("EPRT |2|"));
        assert!(cmd.contains("|2121|"));
    }

    #[test]
    fn parse_unix_mode_from_mlst_facts() {
        let text =
            "250-Listing /pub/file\r\n Type=file;Size=123;UNIX.mode=0644; /pub/file\r\n250 End\r\n";
        assert_eq!(parse_unix_mode_from_mlst(text), Some(0o644));
        assert_eq!(
            parse_unix_mode_from_mlst("unix.mode=0755;type=file; name"),
            Some(0o755)
        );
        assert_eq!(parse_unix_mode_from_mlst("type=file; size=1"), None);
    }

    #[test]
    fn parse_unix_mode_from_list_perms() {
        assert_eq!(
            parse_unix_mode_from_list_line("-rw-r--r-- 1 user group 123 Jan  1 12:00 readme.txt"),
            Some(0o644)
        );
        assert_eq!(
            parse_unix_mode_from_list_line("drwxr-xr-x 2 user group 4096 Jan  1 12:00 docs"),
            Some(0o755)
        );
        assert_eq!(
            parse_unix_mode_from_list_line("-rwxr-xr-x 1 u g 1 Jan 1 00:00 bin"),
            Some(0o755)
        );
        assert_eq!(
            parse_unix_mode_from_list_line("-rwSr--r-- 1 u g 1 Jan 1 00:00 odd"),
            Some(0o4644)
        );
        assert_eq!(
            unix_mode_for_basename(
                "-rw-r--r-- 1 user group 123 Jan  1 12:00 readme.txt\n\
                 drwxr-xr-x 2 user group 4096 Jan  1 12:00 docs\n",
                "readme.txt"
            ),
            Some(0o644)
        );
        assert_eq!(
            unix_mode_for_basename(
                "-rw-r--r-- 1 user group 123 Jan  1 12:00 readme.txt\n",
                "missing.txt"
            ),
            None
        );
    }

    #[test]
    fn parse_mlsd_file_dir_symlink() {
        let text = "\
type=file;size=1; readme.txt
type=dir; docs
type=OS.unix=symlink;UNIX.slink=target.bin; link.bin
type=file; .
type=dir; ..
type=file;size=1; bad/name
";
        let entries = parse_mlsd(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "readme.txt");
        assert_eq!(entries[0].kind, FtpEntryKind::File);
        assert_eq!(entries[1].name, "docs");
        assert_eq!(entries[1].kind, FtpEntryKind::Dir);
        assert_eq!(entries[2].name, "link.bin");
        assert_eq!(
            entries[2].kind,
            FtpEntryKind::Symlink {
                target: Some("target.bin".into())
            }
        );
    }

    #[test]
    fn parse_unix_list_entries() {
        let text = "\
-rw-r--r-- 1 user group 123 Jan  1 12:00 readme.txt
drwxr-xr-x 2 user group 4096 Jan  1 12:00 docs
lrwxrwxrwx 1 user group    4 Jan  1 12:00 link.bin -> target.bin
lrwxrwxrwx 1 user group    5 Jan  1 12:00 other -> ../escape
";
        let entries = parse_unix_list(text);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].kind, FtpEntryKind::File);
        assert_eq!(entries[1].kind, FtpEntryKind::Dir);
        assert_eq!(
            entries[2].kind,
            FtpEntryKind::Symlink {
                target: Some("target.bin".into())
            }
        );
        assert_eq!(
            entries[3].kind,
            FtpEntryKind::Symlink {
                target: Some("../escape".into())
            }
        );
    }

    #[test]
    fn ftps_port_selection() {
        let explicit = Url::parse("ftps://example.com/file").unwrap();
        let cfg = Config::default();
        assert_eq!(ftp_control_port(&explicit, &cfg), 21);

        let cfg_impl = Config {
            ftps_implicit: true,
            ..Config::default()
        };
        assert_eq!(ftp_control_port(&explicit, &cfg_impl), 990);

        let custom = Url::parse("ftps://example.com:2121/file").unwrap();
        assert_eq!(ftp_control_port(&custom, &cfg_impl), 2121);

        let rewritten = Url::parse("ftps://example.com:21/file").unwrap();
        assert_eq!(ftp_control_port(&rewritten, &cfg_impl), 990);

        let plain = Url::parse("ftp://example.com/file").unwrap();
        assert_eq!(ftp_control_port(&plain, &cfg_impl), 21);
    }

    #[test]
    fn ftps_data_tls_respects_clear_data() {
        let cfg = Config::default();
        assert!(ftps_data_tls(true, &cfg));
        assert!(!ftps_data_tls(false, &cfg));
        let clear = Config {
            ftps_clear_data_connection: true,
            ..Config::default()
        };
        assert!(!ftps_data_tls(true, &clear));
    }

    #[test]
    fn auth_tls_reply_codes() {
        assert!(auth_tls_accepted("234 Proceed with negotiation."));
        assert!(auth_tls_accepted("334 Next"));
        assert!(!auth_tls_accepted("500 Unknown command"));
        assert!(!auth_tls_accepted("502 AUTH TLS not supported"));
    }
}
