use encoding_rs::Encoding;

use crate::{Error, Result};

pub fn resolve_encoding(label: &str) -> Result<&'static Encoding> {
    Encoding::for_label(label.trim().as_bytes()).ok_or_else(|| {
        Error::Parse(format!(
            "unknown character encoding '{label}' (use an IANA/WHATWG label such as UTF-8 or ISO-8859-1)"
        ))
    })
}

pub fn decode_bytes(bytes: &[u8], label: Option<&str>) -> Result<String> {
    let encoding = match label {
        Some(l) if !l.trim().is_empty() => resolve_encoding(l)?,
        _ => encoding_rs::UTF_8,
    };
    let (cow, _, _) = encoding.decode(bytes);
    Ok(cow.into_owned())
}

pub fn charset_from_content_type(content_type: &str) -> Option<String> {
    for part in content_type.split(';').skip(1) {
        let part = part.trim();
        let lower = part.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("charset=") {
            let raw = part[part.len() - rest.len()..].trim();
            let raw = raw.trim_matches('"').trim_matches('\'');
            if !raw.is_empty() {
                return Some(raw.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_latin1() {
        let s = decode_bytes(b"caf\xe9", Some("ISO-8859-1")).unwrap();
        assert_eq!(s, "café");
    }

    #[test]
    fn unknown_label_errors() {
        assert!(resolve_encoding("not-a-real-encoding").is_err());
    }

    #[test]
    fn content_type_charset() {
        assert_eq!(
            charset_from_content_type("text/html; charset=ISO-8859-1").as_deref(),
            Some("ISO-8859-1")
        );
        assert_eq!(
            charset_from_content_type(r#"text/html; charset="utf-8""#).as_deref(),
            Some("utf-8")
        );
        assert_eq!(charset_from_content_type("text/html"), None);
    }
}
