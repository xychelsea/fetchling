use fetchling_core::{Error, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use url::Url;

/// A Metalink file mirror.
#[derive(Debug, Clone)]
pub struct MetalinkUrl {
    /// Mirror URL.
    pub url: Url,
    /// Optional country/location code.
    pub location: Option<String>,
    /// Higher values are preferred when location does not decide.
    pub preference: Option<i32>,
}

/// A Metalink digest (`type` + hex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetalinkHash {
    /// Normalized algorithm name (`sha-256`, `sha-1`, …).
    pub algo: String,
    /// Hex-encoded digest.
    pub hex: String,
}

/// A Metalink metaurl (another metalink document).
#[derive(Debug, Clone)]
pub struct MetalinkMetaUrl {
    /// Metaurl location.
    pub url: Url,
    /// Optional media type.
    pub mediatype: Option<String>,
    /// Optional priority.
    pub priority: Option<i32>,
}

/// One file entry from a Metalink document.
#[derive(Debug, Clone)]
pub struct MetalinkFile {
    /// File name from the `name` attribute.
    pub name: String,
    /// Mirror URLs.
    pub urls: Vec<MetalinkUrl>,
    /// Digests for this file.
    pub hashes: Vec<MetalinkHash>,
}

/// Parsed Metalink 3/4 document.
#[derive(Debug, Clone, Default)]
pub struct MetalinkDoc {
    /// File entries.
    pub files: Vec<MetalinkFile>,
    /// Metaurls (links to other metalink documents).
    pub metaurls: Vec<MetalinkMetaUrl>,
}

impl MetalinkFile {
    /// Mirror URLs ordered by preferred location, then descending preference.
    pub fn urls_ordered(&self, preferred_location: Option<&str>) -> Vec<&Url> {
        let mut urls: Vec<&MetalinkUrl> = self.urls.iter().collect();
        urls.sort_by(|a, b| {
            let a_pref = preferred_location
                .map(|want| a.location.as_deref() == Some(want))
                .unwrap_or(false);
            let b_pref = preferred_location
                .map(|want| b.location.as_deref() == Some(want))
                .unwrap_or(false);
            match (a_pref, b_pref) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b
                    .preference
                    .unwrap_or(i32::MIN)
                    .cmp(&a.preference.unwrap_or(i32::MIN)),
            }
        });
        urls.into_iter().map(|u| &u.url).collect()
    }

    /// First URL from [`Self::urls_ordered`].
    pub fn pick_url(&self, preferred_location: Option<&str>) -> Option<&Url> {
        self.urls_ordered(preferred_location).into_iter().next()
    }

    /// Hex SHA-256 digest when present.
    pub fn sha256(&self) -> Option<&str> {
        self.hashes
            .iter()
            .find(|h| normalize_hash_algo(&h.algo) == "sha-256")
            .map(|h| h.hex.as_str())
    }
}

impl MetalinkDoc {
    /// Pick a metalink-typed metaurl.
    ///
    /// `index` is 1-based. Values `<= 0` return the first matching metaurl.
    pub fn select_metaurl(&self, index: i32) -> Option<&MetalinkMetaUrl> {
        let metalink_metas: Vec<_> = self
            .metaurls
            .iter()
            .filter(|m| {
                m.mediatype
                    .as_deref()
                    .map(is_metalink_mediatype)
                    .unwrap_or(true)
            })
            .collect();
        if metalink_metas.is_empty() {
            return None;
        }
        if index <= 0 {
            return metalink_metas.first().copied();
        }
        metalink_metas
            .get((index as usize).saturating_sub(1))
            .copied()
    }
}

/// Whether `ct` is `application/metalink4+xml` or `application/metalink+xml`.
pub fn is_metalink_mediatype(ct: &str) -> bool {
    let ct = ct
        .split(';')
        .next()
        .unwrap_or(ct)
        .trim()
        .to_ascii_lowercase();
    ct == "application/metalink4+xml" || ct == "application/metalink+xml"
}

pub(crate) fn normalize_hash_algo(algo: &str) -> String {
    let a = algo.trim().to_ascii_lowercase().replace('_', "-");
    match a.as_str() {
        "sha256" | "sha-256" => "sha-256".into(),
        "sha1" | "sha-1" => "sha-1".into(),
        "sha512" | "sha-512" => "sha-512".into(),
        "md5" => "md5".into(),
        other => other.to_string(),
    }
}

/// Parse Metalink 3/4 XML and return the file list.
///
/// # Errors
///
/// Returns [`Error::Parse`] when the XML is malformed.
///
/// # Examples
///
/// ```
/// use fetchling_formats::parse_metalink;
///
/// let xml = r#"<?xml version="1.0"?>
/// <metalink xmlns="urn:ietf:params:xml:ns:metalink">
///   <file name="hello.txt">
///     <url>http://example.com/hello.txt</url>
///     <hash type="sha-256">abc</hash>
///   </file>
/// </metalink>"#;
/// let files = parse_metalink(xml).unwrap();
/// assert_eq!(files[0].name, "hello.txt");
/// ```
pub fn parse_metalink(xml: &str) -> Result<Vec<MetalinkFile>> {
    Ok(parse_metalink_doc(xml)?.files)
}

/// Parse Metalink 3/4 XML into files and metaurls.
///
/// # Errors
///
/// Returns [`Error::Parse`] when the XML is malformed.
pub fn parse_metalink_doc(xml: &str) -> Result<MetalinkDoc> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut doc = MetalinkDoc::default();
    let mut current: Option<MetalinkFile> = None;
    let mut in_url = false;
    let mut pending_location: Option<String> = None;
    let mut pending_preference: Option<i32> = None;
    let mut in_hash = false;
    let mut hash_type = String::new();
    let mut in_metaurl = false;
    let mut meta_mediatype: Option<String> = None;
    let mut meta_priority: Option<i32> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let local = name.rsplit(':').next().unwrap_or(&name);
                match local {
                    "file" => {
                        let mut fname = String::new();
                        for a in e.attributes().flatten() {
                            if local_attr_key(a.key.as_ref()) == b"name" {
                                fname = String::from_utf8_lossy(&a.value).to_string();
                            }
                        }
                        current = Some(MetalinkFile {
                            name: fname,
                            urls: Vec::new(),
                            hashes: Vec::new(),
                        });
                    }
                    "url" => {
                        in_url = true;
                        pending_location = None;
                        pending_preference = None;
                        for a in e.attributes().flatten() {
                            let key = local_attr_key(a.key.as_ref());
                            if key == b"location" {
                                pending_location =
                                    Some(String::from_utf8_lossy(&a.value).to_string());
                            } else if key == b"preference" || key == b"priority" {
                                pending_preference = String::from_utf8_lossy(&a.value).parse().ok();
                            }
                        }
                    }
                    "hash" => {
                        in_hash = true;
                        hash_type.clear();
                        for a in e.attributes().flatten() {
                            if local_attr_key(a.key.as_ref()) == b"type" {
                                hash_type = String::from_utf8_lossy(&a.value).to_string();
                            }
                        }
                    }
                    "metaurl" => {
                        in_metaurl = true;
                        meta_mediatype = None;
                        meta_priority = None;
                        for a in e.attributes().flatten() {
                            let key = local_attr_key(a.key.as_ref());
                            if key == b"mediatype" || key == b"type" {
                                meta_mediatype =
                                    Some(String::from_utf8_lossy(&a.value).to_string());
                            } else if key == b"priority" || key == b"preference" {
                                meta_priority = String::from_utf8_lossy(&a.value).parse().ok();
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                let text = t.xml10_content().unwrap_or_default().to_string();
                if in_url {
                    if let Some(f) = current.as_mut() {
                        if let Ok(u) = Url::parse(&text) {
                            f.urls.push(MetalinkUrl {
                                url: u,
                                location: pending_location.take(),
                                preference: pending_preference.take(),
                            });
                        }
                    }
                }
                if in_hash {
                    let algo = normalize_hash_algo(&hash_type);
                    if matches!(algo.as_str(), "sha-256" | "sha-1" | "sha-512" | "md5") {
                        if let Some(f) = current.as_mut() {
                            f.hashes.push(MetalinkHash {
                                algo,
                                hex: text.trim().to_string(),
                            });
                        }
                    }
                }
                if in_metaurl {
                    if let Ok(u) = Url::parse(text.trim()) {
                        doc.metaurls.push(MetalinkMetaUrl {
                            url: u,
                            mediatype: meta_mediatype.take(),
                            priority: meta_priority.take(),
                        });
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let local = name.rsplit(':').next().unwrap_or(&name);
                match local {
                    "file" => {
                        if let Some(f) = current.take() {
                            doc.files.push(f);
                        }
                    }
                    "url" => {
                        in_url = false;
                        pending_location = None;
                        pending_preference = None;
                    }
                    "hash" => in_hash = false,
                    "metaurl" => {
                        in_metaurl = false;
                        meta_mediatype = None;
                        meta_priority = None;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::Parse(format!("metalink XML: {e}"))),
            _ => {}
        }
        buf.clear();
    }
    Ok(doc)
}

fn local_attr_key(key: &[u8]) -> &[u8] {
    if let Some(i) = key.iter().rposition(|b| *b == b':') {
        &key[i + 1..]
    } else {
        key
    }
}

/// A parsed RFC 8288-ish HTTP `Link` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetalinkLink {
    /// Link target (inside `<…>`).
    pub url: String,
    /// `rel` parameter (lowercased).
    pub rel: String,
    /// `type` parameter.
    pub media_type: Option<String>,
    /// `pri` / `priority` parameter.
    pub pri: Option<i32>,
    /// `digest` parameters decoded to [`MetalinkHash`].
    pub digests: Vec<MetalinkHash>,
}

/// Parse each `Link` header value with [`parse_link_header`].
pub fn parse_link_headers(headers: &[String]) -> Vec<MetalinkLink> {
    let mut out = Vec::new();
    for h in headers {
        out.extend(parse_link_header(h));
    }
    out
}

/// Parse one `Link` header (`describedby` / `duplicate` / digests).
pub fn parse_link_header(header: &str) -> Vec<MetalinkLink> {
    let mut out = Vec::new();
    for part in split_link_values(header) {
        if let Some(link) = parse_one_link(part.trim()) {
            out.push(link);
        }
    }
    out
}

fn split_link_values(header: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let bytes = header.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&header[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < header.len() {
        parts.push(&header[start..]);
    }
    parts
}

fn parse_one_link(part: &str) -> Option<MetalinkLink> {
    let part = part.trim();
    let (url, rest) = {
        let rest = part.strip_prefix('<')?;
        let end = rest.find('>')?;
        (&rest[..end], rest[end + 1..].trim_start_matches(';').trim())
    };
    let mut rel = String::new();
    let mut media_type = None;
    let mut pri = None;
    let mut digests = Vec::new();
    for param in rest.split(';') {
        let param = param.trim();
        if param.is_empty() {
            continue;
        }
        let (k, v) = match param.split_once('=') {
            Some((k, v)) => (k.trim(), unquote(v.trim())),
            None => continue,
        };
        let key = k.to_ascii_lowercase();
        match key.as_str() {
            "rel" => rel = v.to_ascii_lowercase(),
            "type" => media_type = Some(v.to_string()),
            "pri" | "priority" => pri = v.parse().ok(),
            "digest" => {
                if let Some((algo, b64)) = v.split_once('=') {
                    if let Ok(bytes) = base64_decode(b64.trim()) {
                        digests.push(MetalinkHash {
                            algo: normalize_hash_algo(algo),
                            hex: hex::encode(bytes),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if rel.is_empty() {
        return None;
    }
    Some(MetalinkLink {
        url: url.to_string(),
        rel,
        media_type,
        pri,
        digests,
    })
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

fn base64_decode(s: &str) -> std::result::Result<Vec<u8>, ()> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(s.trim()))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim()))
        .map_err(|_| ())
}

/// Encode hashes as `algo=hex,...`.
pub fn encode_hashes(hashes: &[MetalinkHash]) -> String {
    hashes
        .iter()
        .map(|h| format!("{}={}", h.algo, h.hex))
        .collect::<Vec<_>>()
        .join(",")
}

/// Decode `algo=hex,...` into [`MetalinkHash`] values.
pub fn decode_hashes(s: &str) -> Vec<MetalinkHash> {
    s.split(',')
        .filter_map(|part| {
            let (algo, hex) = part.split_once('=')?;
            Some(MetalinkHash {
                algo: normalize_hash_algo(algo),
                hex: hex.trim().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple() {
        let xml = r#"<?xml version="1.0"?>
        <metalink xmlns="urn:ietf:params:xml:ns:metalink">
          <file name="hello.txt">
            <url>http://example.com/hello.txt</url>
            <hash type="sha-256">abc</hash>
          </file>
        </metalink>"#;
        let files = parse_metalink(xml).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "hello.txt");
        assert_eq!(
            files[0].urls[0].url.as_str(),
            "http://example.com/hello.txt"
        );
        assert_eq!(files[0].sha256(), Some("abc"));
    }

    #[test]
    fn preferred_location_wins() {
        let xml = r#"<?xml version="1.0"?>
        <metalink xmlns="urn:ietf:params:xml:ns:metalink">
          <file name="f.bin">
            <url location="us">http://us.example.com/f.bin</url>
            <url location="de" preference="10">http://de.example.com/f.bin</url>
          </file>
        </metalink>"#;
        let files = parse_metalink(xml).unwrap();
        let f = &files[0];
        assert_eq!(
            f.pick_url(Some("de")).unwrap().as_str(),
            "http://de.example.com/f.bin"
        );
        assert_eq!(
            f.pick_url(Some("us")).unwrap().as_str(),
            "http://us.example.com/f.bin"
        );
        assert_eq!(
            f.pick_url(None).unwrap().as_str(),
            "http://de.example.com/f.bin"
        );
        assert_eq!(
            f.pick_url(Some("jp")).unwrap().as_str(),
            "http://de.example.com/f.bin"
        );
        let ordered = f.urls_ordered(Some("us"));
        assert_eq!(ordered[0].as_str(), "http://us.example.com/f.bin");
        assert_eq!(ordered[1].as_str(), "http://de.example.com/f.bin");
    }

    #[test]
    fn higher_preference_wins_without_location() {
        let xml = r#"<?xml version="1.0"?>
        <metalink xmlns="urn:ietf:params:xml:ns:metalink">
          <file name="f.bin">
            <url preference="1">http://a.example.com/f.bin</url>
            <url preference="50">http://b.example.com/f.bin</url>
            <url>http://c.example.com/f.bin</url>
          </file>
        </metalink>"#;
        let files = parse_metalink(xml).unwrap();
        assert_eq!(
            files[0].pick_url(None).unwrap().as_str(),
            "http://b.example.com/f.bin"
        );
    }

    #[test]
    fn parse_metaurls_and_multi_hash() {
        let xml = r#"<?xml version="1.0"?>
        <metalink xmlns="urn:ietf:params:xml:ns:metalink">
          <file name="f.bin">
            <url>http://example.com/f.bin</url>
            <hash type="md5">aa</hash>
            <hash type="sha-1">bb</hash>
            <hash type="sha-256">cc</hash>
          </file>
          <metaurl mediatype="application/metalink4+xml" priority="1">http://example.com/a.meta4</metaurl>
          <metaurl mediatype="application/metalink4+xml">http://example.com/b.meta4</metaurl>
        </metalink>"#;
        let doc = parse_metalink_doc(xml).unwrap();
        assert_eq!(doc.files[0].hashes.len(), 3);
        assert_eq!(doc.metaurls.len(), 2);
        assert_eq!(
            doc.select_metaurl(1).unwrap().url.as_str(),
            "http://example.com/a.meta4"
        );
        assert_eq!(
            doc.select_metaurl(2).unwrap().url.as_str(),
            "http://example.com/b.meta4"
        );
        assert!(doc.select_metaurl(9).is_none());
    }

    #[test]
    fn parse_link_describedby_and_duplicate() {
        let header = r#"<http://example.com/f.meta4>; rel=describedby; type="application/metalink4+xml", <http://mirror.example.com/f.bin>; rel=duplicate; pri=1; digest=SHA-256=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="#;
        let links = parse_link_header(header);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].rel, "describedby");
        assert_eq!(links[0].url, "http://example.com/f.meta4");
        assert!(links[0]
            .media_type
            .as_deref()
            .is_some_and(is_metalink_mediatype));
        assert_eq!(links[1].rel, "duplicate");
        assert_eq!(links[1].pri, Some(1));
        assert_eq!(links[1].digests.len(), 1);
        assert_eq!(links[1].digests[0].algo, "sha-256");
    }

    #[test]
    fn encode_decode_hashes_and_normalize() {
        let hashes = vec![MetalinkHash {
            algo: "sha-256".into(),
            hex: "abc".into(),
        }];
        assert_eq!(encode_hashes(&hashes), "sha-256=abc");
        let decoded = decode_hashes("sha256=aa,sha_256=bb,bad,foo=bar");
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].algo, "sha-256");
        assert_eq!(decoded[0].hex, "aa");
        assert_eq!(decoded[1].algo, "sha-256");
        assert_eq!(decoded[2].algo, "foo");
        assert_eq!(normalize_hash_algo("sha256"), "sha-256");
        assert_eq!(normalize_hash_algo("SHA-1"), "sha-1");
        assert_eq!(normalize_hash_algo("md5"), "md5");
        assert_eq!(normalize_hash_algo("blake2"), "blake2");
    }

    #[test]
    fn parse_metalink_rejects_bad_xml() {
        let err = parse_metalink("<metalink><file").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    #[test]
    fn mediatype_link_headers_and_select_metaurl() {
        assert!(is_metalink_mediatype(
            "application/metalink4+xml; charset=utf-8"
        ));
        assert!(!is_metalink_mediatype("text/html"));
        let links = parse_link_headers(&[
            "<http://a.example/f.meta4>; rel=describedby; type=\"application/metalink4+xml\""
                .into(),
            "<http://b.example/f.bin>; rel=duplicate; pri=2".into(),
        ]);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].rel, "describedby");
        assert_eq!(links[1].rel, "duplicate");
        let xml = r#"<?xml version="1.0"?>
        <metalink xmlns="urn:ietf:params:xml:ns:metalink">
          <metaurl mediatype="application/metalink4+xml">http://example.com/a.meta4</metaurl>
          <metaurl mediatype="application/metalink4+xml">http://example.com/b.meta4</metaurl>
        </metalink>"#;
        let doc = parse_metalink_doc(xml).unwrap();
        assert_eq!(
            doc.select_metaurl(0).unwrap().url.as_str(),
            "http://example.com/a.meta4"
        );
        assert_eq!(
            doc.select_metaurl(-1).unwrap().url.as_str(),
            "http://example.com/a.meta4"
        );
    }
}
