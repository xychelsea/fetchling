use std::path::{Path, PathBuf};

use fetchling_core::{Config, Result};
use percent_encoding::percent_decode_str;
use url::Url;

/// Whether to build a host/path directory tree for this retrieval.
///
/// A simple one-shot download lands as a basename in the cwd;
/// recursive / page-requisites builds hierarchy; `-x` / `--force-directories`
/// forces hierarchy even for a single file; `-nd` / `--no-directories` flattens.
fn use_directory_hierarchy(cfg: &Config) -> bool {
    if cfg.force_directories {
        return true;
    }
    if !cfg.directories {
        return false;
    }
    cfg.recursive || cfg.page_requisites
}

pub fn local_path_for_url(cfg: &Config, url: &Url) -> PathBuf {
    if let Some(out) = &cfg.output_document {
        return PathBuf::from(out);
    }

    let url = fetchling_core::strip_query_vars(url, cfg.cut_file_get_vars.as_deref());
    let path = url.path();
    let mut segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let cut = cfg.cut_dirs as usize;
    if cut < segs.len() {
        segs = segs[cut..].to_vec();
    } else {
        segs.clear();
    }

    let file_name = if path.ends_with('/') || segs.is_empty() {
        cfg.default_page.clone()
    } else {
        let last = segs.pop().unwrap_or("index.html");
        decode_component(last)
    };

    if !use_directory_hierarchy(cfg) {
        return Path::new(&cfg.directory_prefix).join(sanitize_component(cfg, &file_name));
    }

    let mut parts: Vec<String> = Vec::new();
    if cfg.protocol_directories {
        parts.push(url.scheme().to_string());
    }
    if cfg.host_directories {
        if let Some(host) = url.host_str() {
            let host = if let Some(port) = url.port() {
                format!("{host}:{port}")
            } else {
                host.to_string()
            };
            parts.push(sanitize_component(cfg, &host));
        }
    }
    for s in segs {
        parts.push(sanitize_component(cfg, &decode_component(s)));
    }

    let mut full = PathBuf::from(&cfg.directory_prefix);
    for p in parts {
        full.push(p);
    }
    full.push(sanitize_component(cfg, &file_name));
    full
}

fn decode_component(s: &str) -> String {
    percent_decode_str(s)
        .decode_utf8()
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| s.to_string())
}

fn sanitize_component(cfg: &Config, name: &str) -> String {
    let unix = cfg.restrict_file_names.iter().any(|m| m == "unix");
    let windows = cfg.restrict_file_names.iter().any(|m| m == "windows");
    let ascii = cfg.restrict_file_names.iter().any(|m| m == "ascii");
    let nocontrol = cfg.restrict_file_names.iter().any(|m| m == "nocontrol");
    let lower = cfg.restrict_file_names.iter().any(|m| m == "lowercase");
    let upper = cfg.restrict_file_names.iter().any(|m| m == "uppercase");

    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        let control = nocontrol && (ch <= '\u{001f}' || ch == '\u{007f}');
        let bad = ch == '\0'
            || control
            || (unix && (ch == '/' || ch == '\n'))
            || (windows && r#"<>:"\|?*"#.contains(ch))
            || (ascii && !ch.is_ascii());
        if bad {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    if lower {
        out = out.to_lowercase();
    } else if upper {
        out = out.to_uppercase();
    }
    if out.is_empty() || out == "." || out == ".." {
        return "_".into();
    }
    out
}

pub fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Result of collision resolution for a preferred download path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestAction {
    /// `--no-clobber`: do not retrieve.
    Skip,
    Path(PathBuf),
}

/// First free path among `path`, `path.1`, `path.2`, ….
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let base = path.display().to_string();
    let mut n = 1u64;
    loop {
        let candidate = PathBuf::from(format!("{base}.{n}"));
        if !candidate.exists() {
            return candidate;
        }
        n = n.saturating_add(1);
        if n == 0 {
            // Exhausted u64 — fall back to preferred (caller will overwrite).
            return path.to_path_buf();
        }
    }
}

/// Resolve where to write given collision policy (call while holding the path lock).
///
/// Default: overwrite the preferred path. `--unique-names` is a
/// fetchling extension that picks `path.1`, `path.2`, … instead.
pub fn resolve_dest_path(cfg: &Config, preferred: &Path) -> DestAction {
    if cfg.output_document.is_some() {
        return DestAction::Path(preferred.to_path_buf());
    }
    if cfg.no_clobber && preferred.exists() {
        return DestAction::Skip;
    }
    if cfg.continue_download && preferred.exists() {
        return DestAction::Path(preferred.to_path_buf());
    }
    if cfg.unique_names && preferred.exists() {
        return DestAction::Path(unique_path(preferred));
    }
    DestAction::Path(preferred.to_path_buf())
}

pub fn should_skip_clobber(cfg: &Config, path: &Path) -> bool {
    matches!(resolve_dest_path(cfg, path), DestAction::Skip)
}

/// Rotate backups `.1` .. `.N` before overwrite when `cfg.backups > 0`.
pub fn rotate_backups(cfg: &Config, path: &Path) -> Result<()> {
    if cfg.backups == 0 || !path.exists() {
        return Ok(());
    }
    let n = cfg.backups;
    for i in (1..=n).rev() {
        let src = if i == 1 {
            path.to_path_buf()
        } else {
            PathBuf::from(format!("{}.{}", path.display(), i - 1))
        };
        let dst = PathBuf::from(format!("{}.{}", path.display(), i));
        if src.exists() {
            let _ = std::fs::rename(&src, &dst);
        }
    }
    Ok(())
}

/// Post-download rename for trust-server-names / Content-Disposition / -E.
///
/// No-op when `-O` is set, dest is stdout, or status is 304.
pub fn finalize_download_path(
    cfg: &Config,
    request_url: &Url,
    dest: &Path,
    final_url: &Url,
    content_type: Option<&str>,
    content_disposition_filename: Option<&str>,
    status: u16,
) -> Result<PathBuf> {
    if cfg.output_document.is_some() || dest.as_os_str() == "-" || status == 304 {
        return Ok(dest.to_path_buf());
    }
    if !cfg.trust_server_names && content_disposition_filename.is_none() && !cfg.adjust_extension {
        return Ok(dest.to_path_buf());
    }

    let mut preferred = if cfg.trust_server_names && final_url.as_str() != request_url.as_str() {
        local_path_for_url(cfg, final_url)
    } else {
        // Rebuild from request URL directory layout using current dest's... actually keep
        // directory of dest and only change basename when not trusting server names.
        dest.to_path_buf()
    };

    if let Some(cd) = content_disposition_filename {
        let name = sanitize_component(cfg, cd);
        preferred = replace_file_name(&preferred, &name);
    }

    if cfg.adjust_extension {
        if let Some(ext) = extension_for_content_type(content_type) {
            preferred = ensure_extension(&preferred, ext);
        }
    }

    if preferred == dest {
        return Ok(dest.to_path_buf());
    }

    let target = match resolve_dest_path(cfg, &preferred) {
        DestAction::Skip => return Ok(dest.to_path_buf()),
        DestAction::Path(p) => p,
    };
    if target == dest {
        return Ok(dest.to_path_buf());
    }
    ensure_parent(&target)?;
    std::fs::rename(dest, &target)?;
    Ok(target)
}

fn replace_file_name(path: &Path, name: &str) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

fn extension_for_content_type(ct: Option<&str>) -> Option<&'static str> {
    let ct = ct?.split(';').next()?.trim().to_ascii_lowercase();
    if ct == "text/html" || ct == "application/xhtml+xml" {
        Some(".html")
    } else if ct == "text/css" {
        Some(".css")
    } else {
        None
    }
}

fn ensure_extension(path: &Path, ext: &str) -> PathBuf {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("index");
    let lower = name.to_ascii_lowercase();
    if ext == ".html" && (lower.ends_with(".html") || lower.ends_with(".htm")) {
        return path.to_path_buf();
    }
    if ext == ".css" && lower.ends_with(".css") {
        return path.to_path_buf();
    }
    replace_file_name(path, &format!("{name}{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchling_core::Config;

    #[test]
    fn simple_download_is_flat_basename() {
        let cfg = Config::default();
        let url = Url::parse("http://example.com/a/b.txt").unwrap();
        let p = local_path_for_url(&cfg, &url);
        assert_eq!(p, PathBuf::from("./b.txt"));
    }

    #[test]
    fn recursive_creates_host_and_path_dirs() {
        let cfg = Config {
            recursive: true,
            ..Config::default()
        };
        let url = Url::parse("http://example.com/a/b.txt").unwrap();
        let p = local_path_for_url(&cfg, &url);
        assert_eq!(p, PathBuf::from("./example.com/a/b.txt"));
    }

    #[test]
    fn force_directories_for_single_download() {
        let cfg = Config {
            force_directories: true,
            ..Config::default()
        };
        let url = Url::parse("http://example.com/a/b.txt").unwrap();
        let p = local_path_for_url(&cfg, &url);
        assert_eq!(p, PathBuf::from("./example.com/a/b.txt"));
    }

    #[test]
    fn no_directories_flattens_recursive() {
        let cfg = Config {
            recursive: true,
            directories: false,
            ..Config::default()
        };
        let url = Url::parse("http://example.com/a/b.txt").unwrap();
        let p = local_path_for_url(&cfg, &url);
        assert_eq!(p, PathBuf::from("./b.txt"));
    }

    #[test]
    fn no_host_directories_keeps_path_dirs() {
        let cfg = Config {
            recursive: true,
            host_directories: false,
            ..Config::default()
        };
        let url = Url::parse("http://example.com/a/b.txt").unwrap();
        let p = local_path_for_url(&cfg, &url);
        assert_eq!(p, PathBuf::from("./a/b.txt"));
    }

    #[test]
    fn output_document_overrides() {
        let cfg = Config {
            output_document: Some("out.bin".into()),
            ..Config::default()
        };
        let url = Url::parse("http://example.com/a/b.txt").unwrap();
        let p = local_path_for_url(&cfg, &url);
        assert_eq!(p, PathBuf::from("out.bin"));
    }

    #[test]
    fn sanitize_maps_dot_and_dotdot() {
        let cfg = Config::default();
        assert_eq!(sanitize_component(&cfg, "."), "_");
        assert_eq!(sanitize_component(&cfg, ".."), "_");
        assert_eq!(sanitize_component(&cfg, ""), "_");
    }

    #[test]
    fn sanitize_nocontrol_strips_controls() {
        let cfg = Config {
            restrict_file_names: vec!["nocontrol".into()],
            ..Config::default()
        };
        assert_eq!(sanitize_component(&cfg, "a\nb\x7fc"), "a_b_c");
        assert_eq!(sanitize_component(&cfg, "ok-name"), "ok-name");
    }

    #[test]
    fn hierarchy_dotdot_components_cannot_escape_prefix() {
        let cfg = Config {
            recursive: true,
            directory_prefix: "./downloads".into(),
            ..Config::default()
        };
        // Simulate decoded path segments that would escape without sanitization.
        let mut full = PathBuf::from(&cfg.directory_prefix);
        full.push(sanitize_component(&cfg, "example.com"));
        full.push(sanitize_component(&cfg, "a"));
        full.push(sanitize_component(&cfg, ".."));
        full.push(sanitize_component(&cfg, ".."));
        full.push(sanitize_component(&cfg, "etc"));
        full.push(sanitize_component(&cfg, "passwd"));
        assert_eq!(
            full,
            PathBuf::from("./downloads/example.com/a/_/_/etc/passwd")
        );
        assert!(!full
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)));
    }

    #[test]
    fn unique_path_picks_numbered_suffix() {
        let dir = std::env::temp_dir().join(format!("fetchling-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("file.bin");
        assert_eq!(unique_path(&base), base);
        std::fs::write(&base, b"a").unwrap();
        assert_eq!(
            unique_path(&base),
            PathBuf::from(format!("{}.1", base.display()))
        );
        std::fs::write(dir.join("file.bin.1"), b"b").unwrap();
        assert_eq!(
            unique_path(&base),
            PathBuf::from(format!("{}.2", base.display()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_dest_path_policies() {
        let dir = std::env::temp_dir().join(format!("fetchling-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let preferred = dir.join("doc.bin");

        let free = resolve_dest_path(&Config::default(), &preferred);
        assert_eq!(free, DestAction::Path(preferred.clone()));

        std::fs::write(&preferred, b"x").unwrap();
        assert_eq!(
            resolve_dest_path(&Config::default(), &preferred),
            DestAction::Path(preferred.clone())
        );

        let skip_cfg = Config {
            no_clobber: true,
            ..Config::default()
        };
        assert_eq!(resolve_dest_path(&skip_cfg, &preferred), DestAction::Skip);

        let unique_cfg = Config {
            unique_names: true,
            ..Config::default()
        };
        assert_eq!(
            resolve_dest_path(&unique_cfg, &preferred),
            DestAction::Path(PathBuf::from(format!("{}.1", preferred.display())))
        );

        let out_cfg = Config {
            output_document: Some(preferred.display().to_string()),
            no_clobber: true,
            ..Config::default()
        };
        assert_eq!(
            resolve_dest_path(&out_cfg, &preferred),
            DestAction::Path(preferred.clone())
        );

        let cont_cfg = Config {
            continue_download: true,
            ..Config::default()
        };
        assert_eq!(
            resolve_dest_path(&cont_cfg, &preferred),
            DestAction::Path(preferred.clone())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_adjust_extension_html() {
        let dir = std::env::temp_dir().join(format!("fetchling-adj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("page");
        std::fs::write(&dest, b"<html>").unwrap();
        let cfg = Config {
            adjust_extension: true,
            ..Config::default()
        };
        let url = Url::parse("http://example.com/page").unwrap();
        let out =
            finalize_download_path(&cfg, &url, &dest, &url, Some("text/html"), None, 200).unwrap();
        assert_eq!(out, dir.join("page.html"));
        assert!(out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_content_disposition_basename() {
        let dir = std::env::temp_dir().join(format!("fetchling-cd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("dl.bin");
        std::fs::write(&dest, b"x").unwrap();
        let cfg = Config {
            content_disposition: true,
            ..Config::default()
        };
        let url = Url::parse("http://example.com/dl.bin").unwrap();
        let out =
            finalize_download_path(&cfg, &url, &dest, &url, None, Some("report.pdf"), 200).unwrap();
        assert_eq!(out, dir.join("report.pdf"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_skips_output_document() {
        let cfg = Config {
            output_document: Some("out".into()),
            adjust_extension: true,
            ..Config::default()
        };
        let url = Url::parse("http://example.com/x").unwrap();
        let dest = PathBuf::from("out");
        let out =
            finalize_download_path(&cfg, &url, &dest, &url, Some("text/html"), None, 200).unwrap();
        assert_eq!(out, dest);
    }

    #[test]
    fn finalize_trust_server_names() {
        let dir = std::env::temp_dir().join(format!("fetchling-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("start.bin");
        std::fs::write(&dest, b"x").unwrap();
        let cfg = Config {
            trust_server_names: true,
            directory_prefix: dir.display().to_string(),
            ..Config::default()
        };
        let start = Url::parse("http://example.com/start.bin").unwrap();
        let final_u = Url::parse("http://example.com/final.bin").unwrap();
        let out = finalize_download_path(&cfg, &start, &dest, &final_u, None, None, 200).unwrap();
        assert_eq!(out.file_name().unwrap(), "final.bin");
        assert!(out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
