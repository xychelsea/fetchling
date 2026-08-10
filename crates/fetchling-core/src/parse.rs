use crate::{Error, Result};

/// Parsed byte size supporting k/m/g/t suffixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ByteSize(pub u64);

impl ByteSize {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Parse byte amounts (`20k`, `1m`, plain integers).
pub fn parse_bytes(s: &str) -> Result<ByteSize> {
    let s = s.trim();
    if s.is_empty() {
        return Err(Error::Parse("empty byte size".into()));
    }
    let (num, mult) = match s.as_bytes().last().map(|b| b.to_ascii_lowercase()) {
        Some(b @ (b'k' | b'm' | b'g' | b't')) => {
            let mult = match b {
                b'k' => 1024u64,
                b'm' => 1024 * 1024,
                b'g' => 1024 * 1024 * 1024,
                b't' => 1024u64 * 1024 * 1024 * 1024,
                _ => unreachable!(),
            };
            (&s[..s.len() - 1], mult)
        }
        _ => (s, 1u64),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| Error::Parse(format!("invalid byte size: {s}")))?;
    Ok(ByteSize(n.saturating_mul(mult)))
}

/// Parse durations (`30`, `1m`, `2h`, `1d`).
pub fn parse_seconds(s: &str) -> Result<f64> {
    let s = s.trim();
    if s.is_empty() {
        return Err(Error::Parse("empty duration".into()));
    }
    let (num, mult) = match s.as_bytes().last().map(|b| b.to_ascii_lowercase()) {
        Some(b @ (b's' | b'm' | b'h' | b'd')) => {
            let mult = match b {
                b's' => 1.0,
                b'm' => 60.0,
                b'h' => 3600.0,
                b'd' => 86400.0,
                _ => unreachable!(),
            };
            (&s[..s.len() - 1], mult)
        }
        _ => (s, 1.0),
    };
    let n: f64 = num
        .parse()
        .map_err(|_| Error::Parse(format!("invalid duration: {s}")))?;
    Ok(n * mult)
}

/// Parse tries (`0` / `inf` = infinite, represented as 0).
pub fn parse_tries(s: &str) -> Result<u32> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("inf") || s == "0" {
        return Ok(0);
    }
    s.parse()
        .map_err(|_| Error::Parse(format!("invalid tries: {s}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_suffixes() {
        assert_eq!(parse_bytes("20k").unwrap().0, 20 * 1024);
        assert_eq!(parse_bytes("1m").unwrap().0, 1024 * 1024);
        assert_eq!(parse_bytes("100").unwrap().0, 100);
    }

    #[test]
    fn duration_suffixes() {
        assert!((parse_seconds("1m").unwrap() - 60.0).abs() < f64::EPSILON);
        assert!((parse_seconds("2h").unwrap() - 7200.0).abs() < f64::EPSILON);
    }
}
