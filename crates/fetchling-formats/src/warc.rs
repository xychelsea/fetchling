use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fetchling_core::{Config, Error, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use sha1::{Digest, Sha1};

pub struct WarcWriter {
    path: PathBuf,
    base_path: PathBuf,
    compress: bool,
    digests: bool,
    max_size: Option<u64>,
    written: u64,
    segment: u32,
    headers: Vec<String>,
    cdx_path: Option<PathBuf>,
    cdx: Option<BufWriter<File>>,
    dedup: HashSet<String>,
}

pub struct WarcWriteInfo {
    pub offset: u64,
    pub digest: Option<String>,
    pub skipped_dedup: bool,
}

impl WarcWriter {
    pub fn open(cfg: &Config) -> Result<Option<Self>> {
        let Some(path) = &cfg.warc_file else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let _ = File::create(&path)?;
        let digests = cfg.warc_digests || cfg.warc_cdx || cfg.warc_dedup.is_some();
        let mut cdx_path = None;
        let mut cdx = None;
        if cfg.warc_cdx {
            let p = cdx_sidecar_path(&path);
            let f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&p)
                .map_err(|e| {
                    Error::Io(std::io::Error::new(
                        e.kind(),
                        format!("cannot open CDX {}: {e}", p.display()),
                    ))
                })?;
            let mut w = BufWriter::new(f);
            writeln!(w, "CDX N b a m s k r M S V g")?;
            cdx_path = Some(p);
            cdx = Some(w);
        }
        let dedup = if let Some(dedup_path) = &cfg.warc_dedup {
            load_dedup_digests(Path::new(dedup_path))?
        } else {
            HashSet::new()
        };
        let mut w = Self {
            base_path: path.clone(),
            path,
            compress: cfg.warc_compression,
            digests,
            max_size: cfg.warc_max_size,
            written: 0,
            segment: 0,
            headers: cfg.warc_header.clone(),
            cdx_path,
            cdx,
            dedup,
        };
        w.write_warcinfo()?;
        Ok(Some(w))
    }

    fn open_writer(&self) -> Result<Box<dyn Write>> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        if self.compress {
            Ok(Box::new(BufWriter::new(GzEncoder::new(
                file,
                Compression::default(),
            ))))
        } else {
            Ok(Box::new(BufWriter::new(file)))
        }
    }

    fn file_offset(&self) -> Result<u64> {
        Ok(std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0))
    }

    fn write_warcinfo(&mut self) -> Result<()> {
        let mut body = String::from("software: fetchling\r\nformat: WARC file version 1.0\r\n");
        for h in &self.headers {
            body.push_str(h);
            body.push_str("\r\n");
        }
        self.write_record(RecordOpts {
            record_type: "warcinfo",
            target_uri: None,
            block_digest: None,
            payload_digest: None,
            content_type: None,
            concurrent_to: None,
            ip_address: None,
            profile: None,
            body: body.as_bytes(),
        })?;
        Ok(())
    }

    fn rotate_if_needed(&mut self, upcoming: usize) -> Result<()> {
        let Some(max) = self.max_size else {
            return Ok(());
        };
        if self.written == 0 || self.written + upcoming as u64 <= max {
            return Ok(());
        }
        self.segment += 1;
        self.path = rotated_path(&self.base_path, self.segment);
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let _ = File::create(&self.path)?;
        self.written = 0;
        self.write_warcinfo()?;
        Ok(())
    }

    pub fn write_response(
        &mut self,
        target_uri: &str,
        http_headers_and_body: &[u8],
        status: u16,
        mime: Option<&str>,
        concurrent_to: Option<&str>,
        ip_address: Option<&str>,
    ) -> Result<WarcWriteInfo> {
        let block_digest = if self.digests {
            Some(sha1_digest(http_headers_and_body))
        } else {
            None
        };
        let payload_digest = if self.digests {
            http_payload(http_headers_and_body).map(sha1_digest)
        } else {
            None
        };
        let dedup_hit = [payload_digest.as_ref(), block_digest.as_ref()]
            .into_iter()
            .flatten()
            .any(|d| self.dedup.contains(d) || self.dedup.contains(&digest_key(d)));
        let record_type = if dedup_hit { "revisit" } else { "response" };
        let offset = self.file_offset()?;
        self.write_record(RecordOpts {
            record_type,
            target_uri: Some(target_uri),
            block_digest: block_digest.as_deref(),
            payload_digest: payload_digest.as_deref(),
            content_type: Some("application/http; msgtype=response"),
            concurrent_to,
            ip_address,
            profile: if dedup_hit {
                Some("http://netpreserve.org/warc/1.0/revisit/identical-payload-digest")
            } else {
                None
            },
            body: http_headers_and_body,
        })?;
        if let Some(cdx) = self.cdx.as_mut() {
            let date = warc_date_compact();
            let mime = mime.and_then(|m| m.split(';').next()).unwrap_or("-").trim();
            let digest_field = block_digest.as_deref().unwrap_or("-");
            let filename = self
                .path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("fetchling.warc");
            writeln!(
                cdx,
                "{target_uri} {date} {target_uri} {mime} {status} {digest_field} - - {} {offset} {filename}",
                http_headers_and_body.len()
            )?;
            cdx.flush()?;
        }
        Ok(WarcWriteInfo {
            offset,
            digest: block_digest,
            skipped_dedup: dedup_hit,
        })
    }

    pub fn write_request(
        &mut self,
        target_uri: &str,
        request: &[u8],
        ip_address: Option<&str>,
    ) -> Result<String> {
        let block_digest = if self.digests {
            Some(sha1_digest(request))
        } else {
            None
        };
        let payload_digest = if self.digests {
            http_payload(request).map(sha1_digest)
        } else {
            None
        };
        self.write_record(RecordOpts {
            record_type: "request",
            target_uri: Some(target_uri),
            block_digest: block_digest.as_deref(),
            payload_digest: payload_digest.as_deref(),
            content_type: Some("application/http; msgtype=request"),
            concurrent_to: None,
            ip_address,
            profile: None,
            body: request,
        })
    }

    pub fn write_resource(&mut self, target_uri: &str, body: &[u8]) -> Result<()> {
        let block_digest = if self.digests {
            Some(sha1_digest(body))
        } else {
            None
        };
        self.write_record(RecordOpts {
            record_type: "resource",
            target_uri: Some(target_uri),
            block_digest: block_digest.as_deref(),
            payload_digest: None,
            content_type: None,
            concurrent_to: None,
            ip_address: None,
            profile: None,
            body,
        })?;
        Ok(())
    }

    fn write_record(&mut self, opts: RecordOpts<'_>) -> Result<String> {
        self.rotate_if_needed(opts.body.len())?;
        let mut w = self.open_writer()?;
        let date = warc_date();
        let id = format!("<urn:uuid:{}>", simple_uuid());
        writeln!(w, "WARC/1.0")?;
        writeln!(w, "WARC-Type: {}", opts.record_type)?;
        writeln!(w, "WARC-Date: {date}")?;
        writeln!(w, "WARC-Record-ID: {id}")?;
        if let Some(uri) = opts.target_uri {
            writeln!(w, "WARC-Target-URI: {uri}")?;
        }
        if let Some(profile) = opts.profile {
            writeln!(w, "WARC-Profile: {profile}")?;
        }
        if let Some(ip) = opts.ip_address {
            writeln!(w, "WARC-IP-Address: {ip}")?;
        }
        if let Some(ct) = opts.content_type {
            writeln!(w, "Content-Type: {ct}")?;
        }
        if let Some(c) = opts.concurrent_to {
            writeln!(w, "WARC-Concurrent-To: {c}")?;
        }
        if let Some(d) = opts.block_digest {
            writeln!(w, "WARC-Block-Digest: {d}")?;
        }
        if let Some(d) = opts.payload_digest {
            writeln!(w, "WARC-Payload-Digest: {d}")?;
        }
        writeln!(w, "Content-Length: {}", opts.body.len())?;
        writeln!(w)?;
        w.write_all(opts.body)?;
        writeln!(w)?;
        writeln!(w)?;
        w.flush()?;
        self.written += opts.body.len() as u64;
        Ok(id)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn cdx_path(&self) -> Option<&Path> {
        self.cdx_path.as_deref()
    }
}

struct RecordOpts<'a> {
    record_type: &'a str,
    target_uri: Option<&'a str>,
    block_digest: Option<&'a str>,
    payload_digest: Option<&'a str>,
    content_type: Option<&'a str>,
    concurrent_to: Option<&'a str>,
    ip_address: Option<&'a str>,
    profile: Option<&'a str>,
    body: &'a [u8],
}

fn http_payload(block: &[u8]) -> Option<&[u8]> {
    let sep = b"\r\n\r\n";
    let pos = block.windows(sep.len()).position(|w| w == sep)?;
    Some(&block[pos + sep.len()..])
}

fn cdx_sidecar_path(warc: &Path) -> PathBuf {
    let name = warc
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("fetchling.warc");
    let stem = name
        .strip_suffix(".warc.gz")
        .or_else(|| name.strip_suffix(".warc"))
        .unwrap_or(name);
    warc.parent()
        .unwrap_or_else(|| Path::new(""))
        .join(format!("{stem}.cdx"))
}

fn load_dedup_digests(path: &Path) -> Result<HashSet<String>> {
    let f = File::open(path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("cannot open warc-dedup {}: {e}", path.display()),
        ))
    })?;
    let mut set = HashSet::new();
    for line in BufReader::new(f).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with("CDX") || line.starts_with('#') {
            continue;
        }
        for field in line.split_whitespace() {
            if let Some(rest) = field.strip_prefix("sha1:") {
                set.insert(format!("sha1:{rest}"));
                set.insert(rest.to_string());
            } else if field.len() == 40 && field.chars().all(|c| c.is_ascii_hexdigit()) {
                set.insert(field.to_ascii_lowercase());
                set.insert(format!("sha1:{}", field.to_ascii_lowercase()));
            }
        }
    }
    Ok(set)
}

fn digest_key(d: &str) -> String {
    d.strip_prefix("sha1:").unwrap_or(d).to_string()
}

fn rotated_path(base: &Path, segment: u32) -> PathBuf {
    let name = base
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("fetchling.warc");
    let (stem, suffix) = if let Some(s) = name.strip_suffix(".warc.gz") {
        (s, ".warc.gz")
    } else if let Some(s) = name.strip_suffix(".warc") {
        (s, ".warc")
    } else {
        (name, "")
    };
    let parent = base.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{stem}-{segment}{suffix}"))
}

fn sha1_digest(data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(data);
    format!("sha1:{}", hex::encode(h.finalize()))
}

fn warc_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = unix_to_utc(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn warc_date_compact() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = unix_to_utc(secs);
    format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}")
}

fn unix_to_utc(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let z = (secs / 86400) as i64 + 719_468;
    let tod = secs % 86400;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let h = (tod / 3600) as u32;
    let mi = ((tod % 3600) / 60) as u32;
    let s = (tod % 60) as u32;
    (y as i32, m as u32, d as u32, h, mi, s)
}

fn simple_uuid() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchling_core::Config;

    #[test]
    fn unix_epoch_date() {
        let (y, mo, d, h, mi, s) = unix_to_utc(0);
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn known_unix_date() {
        let (y, mo, d, h, mi, s) = unix_to_utc(946_684_800);
        assert_eq!((y, mo, d, h, mi, s), (2000, 1, 1, 0, 0, 0));
    }

    #[test]
    fn rotated_path_preserves_suffix() {
        assert_eq!(
            rotated_path(Path::new("/tmp/out.warc.gz"), 1),
            PathBuf::from("/tmp/out-1.warc.gz")
        );
        assert_eq!(
            rotated_path(Path::new("out.warc"), 2),
            PathBuf::from("out-2.warc")
        );
    }

    #[test]
    fn max_size_rotates_to_segment_file() {
        let dir = std::env::temp_dir().join(format!("fetchling-warc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cap.warc");
        let cfg = Config {
            warc_file: Some(path.display().to_string()),
            warc_compression: false,
            warc_max_size: Some(80),
            ..Config::default()
        };
        let mut w = WarcWriter::open(&cfg).unwrap().unwrap();
        w.write_response(
            "http://example.com/",
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            200,
            Some("text/plain"),
            None,
            None,
        )
        .unwrap();
        w.write_response(
            "http://example.com/2",
            b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            200,
            Some("text/plain"),
            None,
            None,
        )
        .unwrap();
        assert!(dir.join("cap-1.warc").exists() || w.path().ends_with("cap-1.warc"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cdx_and_dedup_writes_revisit() {
        let dir = std::env::temp_dir().join(format!("fetchling-warc-cdx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cap.warc");
        let cfg = Config {
            warc_file: Some(path.display().to_string()),
            warc_compression: false,
            warc_cdx: true,
            warc_digests: true,
            ..Config::default()
        };
        let body = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nhello";
        let mut w = WarcWriter::open(&cfg).unwrap().unwrap();
        let info = w
            .write_response(
                "http://example.com/a",
                body,
                200,
                Some("text/plain"),
                None,
                Some("203.0.113.10"),
            )
            .unwrap();
        assert!(!info.skipped_dedup);
        assert!(info.digest.is_some());
        let cdx = std::fs::read_to_string(dir.join("cap.cdx")).unwrap();
        assert!(cdx.contains("http://example.com/a"));
        assert!(cdx.contains(info.digest.as_ref().unwrap()));
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("WARC-IP-Address: 203.0.113.10"));
        assert!(first.contains("WARC-Type: response"));

        let dedup_path = dir.join("prior.cdx");
        std::fs::write(&dedup_path, &cdx).unwrap();
        let path2 = dir.join("cap2.warc");
        let cfg2 = Config {
            warc_file: Some(path2.display().to_string()),
            warc_compression: false,
            warc_dedup: Some(dedup_path.display().to_string()),
            warc_cdx: true,
            warc_digests: true,
            ..Config::default()
        };
        let mut w2 = WarcWriter::open(&cfg2).unwrap().unwrap();
        let info2 = w2
            .write_response(
                "http://example.com/a",
                body,
                200,
                Some("text/plain"),
                None,
                Some("203.0.113.10"),
            )
            .unwrap();
        assert!(info2.skipped_dedup);
        let text = std::fs::read_to_string(&path2).unwrap();
        assert!(text.contains("WARC-Type: revisit"));
        assert!(text.contains(
            "WARC-Profile: http://netpreserve.org/warc/1.0/revisit/identical-payload-digest"
        ));
        assert!(text.contains("WARC-IP-Address: 203.0.113.10"));
        let cdx2 = std::fs::read_to_string(dir.join("cap2.cdx")).unwrap();
        assert!(cdx2.contains("http://example.com/a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_to_and_payload_digest() {
        let dir = std::env::temp_dir().join(format!("fetchling-warc-ct-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cap.warc");
        let cfg = Config {
            warc_file: Some(path.display().to_string()),
            warc_compression: false,
            warc_digests: true,
            ..Config::default()
        };
        let mut w = WarcWriter::open(&cfg).unwrap().unwrap();
        let req = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let resp = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nhello";
        let req_id = w
            .write_request("http://example.com/", req, Some("198.51.100.1"))
            .unwrap();
        w.write_response(
            "http://example.com/",
            resp,
            200,
            Some("text/plain"),
            Some(&req_id),
            Some("198.51.100.1"),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Content-Type: application/http; msgtype=request"));
        assert!(text.contains("Content-Type: application/http; msgtype=response"));
        assert!(text.contains(&format!("WARC-Concurrent-To: {req_id}")));
        assert!(text.contains("WARC-IP-Address: 198.51.100.1"));
        assert!(text.contains("WARC-Block-Digest:"));
        assert!(text.contains("WARC-Payload-Digest:"));
        let block = text
            .lines()
            .find(|l| l.starts_with("WARC-Block-Digest:"))
            .unwrap();
        let payload = text
            .lines()
            .find(|l| l.starts_with("WARC-Payload-Digest:"))
            .unwrap();
        assert_ne!(block, payload);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_resource_record() {
        let dir = std::env::temp_dir().join(format!("fetchling-warc-res-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cap.warc");
        let cfg = Config {
            warc_file: Some(path.display().to_string()),
            warc_compression: false,
            ..Config::default()
        };
        let mut w = WarcWriter::open(&cfg).unwrap().unwrap();
        w.write_resource("metadata://fetchling/log", b"log line\n")
            .unwrap();
        let data = std::fs::read_to_string(&path).unwrap();
        assert!(data.contains("WARC-Type: resource"));
        assert!(data.contains("log line"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
