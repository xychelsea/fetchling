use std::path::{Path, PathBuf};

use fetchling_core::{Config, Result};

#[cfg(doc)]
use fetchling_core::Error;
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

/// Local destination for `url` under `cfg.directory_prefix`.
///
/// A one-shot download is a basename in the prefix. Recursive /
/// page-requisites (or `-x` / `--force-directories`) build a host/path tree.
/// `-O` / `--output-document` overrides the path.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use fetchling_core::Config;
/// use fetchling_engine::local_path_for_url;
/// use url::Url;
///
/// let cfg = Config::default();
/// let url = Url::parse("http://example.com/a/b.txt").unwrap();
/// assert_eq!(local_path_for_url(&cfg, &url), PathBuf::from("./b.txt"));
/// ```
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

/// Create parent directories of `path` when they are missing.
///
/// # Errors
///
/// Returns [`Error::Io`] when a parent directory cannot be created.
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
    /// Write to this path (overwrite, resume, or unique-names).
    Path(PathBuf),
}

/// First free path among `path`, `path.1`, `path.2`, ….
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use fetchling_engine::unique_path;
///
/// let p = Path::new("fetchling-engine-unique-missing.bin");
/// assert_eq!(unique_path(p), p);
/// ```
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
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use fetchling_core::Config;
/// use fetchling_engine::{resolve_dest_path, DestAction};
///
/// let cfg = Config::default();
/// let p = Path::new("fetchling-engine-resolve-missing.bin");
/// assert_eq!(resolve_dest_path(&cfg, p), DestAction::Path(p.to_path_buf()));
/// ```
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

/// Whether [`resolve_dest_path`] is [`DestAction::Skip`].
pub fn should_skip_clobber(cfg: &Config, path: &Path) -> bool {
    matches!(resolve_dest_path(cfg, path), DestAction::Skip)
}

/// Rotate backups `.1` .. `.N` before overwrite when `cfg.backups > 0`.
///
/// # Errors
///
/// Returns [`Error::Io`] when a backup cannot be rotated.
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
///
/// # Errors
///
/// Returns [`Error::Io`] when the file cannot be renamed.
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

    #[test]
    fn local_path_layout_cut_dirs_port_and_decode() {
        let url = Url::parse("http://example.com:8080/a/b%20c.txt").unwrap();
        let page_req = Config {
            page_requisites: true,
            ..Config::default()
        };
        assert_eq!(
            local_path_for_url(&page_req, &url),
            PathBuf::from("./example.com:8080/a/b c.txt")
        );
        let cut = Config {
            recursive: true,
            cut_dirs: 1,
            ..Config::default()
        };
        assert_eq!(
            local_path_for_url(&cut, &url),
            PathBuf::from("./example.com:8080/b c.txt")
        );
        let slash = Url::parse("http://example.com/dir/").unwrap();
        let def = Config {
            recursive: true,
            default_page: "home.html".into(),
            ..Config::default()
        };
        assert_eq!(
            local_path_for_url(&def, &slash),
            PathBuf::from("./example.com/dir/home.html")
        );
        let proto = Config {
            recursive: true,
            protocol_directories: true,
            ..Config::default()
        };
        let simple = Url::parse("http://example.com/a/b.txt").unwrap();
        assert_eq!(
            local_path_for_url(&proto, &simple),
            PathBuf::from("./http/example.com/a/b.txt")
        );
    }

    #[test]
    fn sanitize_unix_windows_ascii_and_case() {
        let unix = Config {
            restrict_file_names: vec!["unix".into()],
            ..Config::default()
        };
        assert_eq!(sanitize_component(&unix, "a/b\nc"), "a_b_c");
        let windows = Config {
            restrict_file_names: vec!["windows".into()],
            ..Config::default()
        };
        assert_eq!(sanitize_component(&windows, r#"a<>:"\|?*b"#), "a________b");
        let ascii = Config {
            restrict_file_names: vec!["ascii".into()],
            ..Config::default()
        };
        assert_eq!(sanitize_component(&ascii, "café"), "caf_");
        let lower = Config {
            restrict_file_names: vec!["lowercase".into()],
            ..Config::default()
        };
        assert_eq!(sanitize_component(&lower, "AbC"), "abc");
        let upper = Config {
            restrict_file_names: vec!["uppercase".into()],
            ..Config::default()
        };
        assert_eq!(sanitize_component(&upper, "AbC"), "ABC");
    }

    #[test]
    fn ensure_parent_creates_nested_and_ignores_basename() {
        let dir =
            std::env::temp_dir().join(format!("fetchling-engine-parent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let nested = dir.join("a").join("b").join("file.bin");
        ensure_parent(&nested).unwrap();
        assert!(dir.join("a").join("b").is_dir());
        ensure_parent(Path::new("file.bin")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_clobber_and_rotate_backups() {
        let dir =
            std::env::temp_dir().join(format!("fetchling-engine-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.bin");
        assert!(!should_skip_clobber(&Config::default(), &path));
        std::fs::write(&path, b"a").unwrap();
        assert!(!should_skip_clobber(&Config::default(), &path));
        let skip = Config {
            no_clobber: true,
            ..Config::default()
        };
        assert!(should_skip_clobber(&skip, &path));
        rotate_backups(&Config::default(), &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"a");
        std::fs::write(dir.join("doc.bin.1"), b"b").unwrap();
        let two = Config {
            backups: 2,
            ..Config::default()
        };
        rotate_backups(&two, &path).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(dir.join("doc.bin.1")).unwrap(), b"a");
        assert_eq!(std::fs::read(dir.join("doc.bin.2")).unwrap(), b"b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_noop_css_and_existing_html_ext() {
        let url = Url::parse("http://example.com/x").unwrap();
        let dash = PathBuf::from("-");
        let cfg = Config {
            adjust_extension: true,
            ..Config::default()
        };
        assert_eq!(
            finalize_download_path(&cfg, &url, &dash, &url, Some("text/css"), None, 200).unwrap(),
            dash
        );
        let dir = std::env::temp_dir().join(format!("fetchling-engine-fin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("page");
        std::fs::write(&dest, b"x").unwrap();
        assert_eq!(
            finalize_download_path(&cfg, &url, &dest, &url, Some("text/html"), None, 304).unwrap(),
            dest
        );
        let css = dir.join("style");
        std::fs::write(&css, b"body{}").unwrap();
        let out =
            finalize_download_path(&cfg, &url, &css, &url, Some("text/css"), None, 200).unwrap();
        assert_eq!(out, dir.join("style.css"));
        let html = dir.join("index.html");
        std::fs::write(&html, b"<html>").unwrap();
        assert_eq!(
            finalize_download_path(&cfg, &url, &html, &url, Some("text/html"), None, 200).unwrap(),
            html
        );
        let htm = dir.join("index.htm");
        std::fs::write(&htm, b"<html>").unwrap();
        assert_eq!(
            finalize_download_path(&cfg, &url, &htm, &url, Some("text/html"), None, 200).unwrap(),
            htm
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
