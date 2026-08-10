use quick_xml::events::Event;
use quick_xml::Reader;
use url::Url;

pub fn extract_rss_urls(base: &Url, xml: &str) -> Vec<Url> {
    let mut out = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_link = false;
    let mut text = String::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local_name(&e);
                if name == "enclosure" {
                    attr_url(base, &e, "url", &mut out);
                } else if name == "link" {
                    in_link = true;
                    text.clear();
                    attr_url(base, &e, "href", &mut out);
                }
            }
            Ok(Event::Empty(e)) => {
                let name = local_name(&e);
                if name == "enclosure" {
                    attr_url(base, &e, "url", &mut out);
                } else if name == "link" {
                    attr_url(base, &e, "href", &mut out);
                }
            }
            Ok(Event::Text(t)) if in_link => {
                text.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::End(e)) => {
                if local_name_end(&e) == "link" {
                    if !text.trim().is_empty() {
                        push_http_url(base, text.trim(), &mut out);
                    }
                    in_link = false;
                    text.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    dedup_urls(out)
}

pub fn extract_atom_urls(base: &Url, xml: &str) -> Vec<Url> {
    let mut out = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if local_name(&e) == "link" {
                    attr_url(base, &e, "href", &mut out);
                }
            }
            Ok(Event::Empty(e)) => {
                if local_name(&e) == "link" {
                    attr_url(base, &e, "href", &mut out);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    dedup_urls(out)
}

pub fn extract_sitemap_urls(base: &Url, xml: &str) -> Vec<Url> {
    let mut out = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_loc = false;
    let mut text = String::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if local_name(&e) == "loc" {
                    in_loc = true;
                    text.clear();
                }
            }
            Ok(Event::Text(t)) if in_loc => {
                text.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::End(e)) => {
                if local_name_end(&e) == "loc" {
                    if !text.trim().is_empty() {
                        push_http_url(base, text.trim(), &mut out);
                    }
                    in_loc = false;
                    text.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    dedup_urls(out)
}

fn local_name(e: &quick_xml::events::BytesStart<'_>) -> String {
    String::from_utf8_lossy(e.local_name().as_ref()).to_ascii_lowercase()
}

fn local_name_end(e: &quick_xml::events::BytesEnd<'_>) -> String {
    String::from_utf8_lossy(e.local_name().as_ref()).to_ascii_lowercase()
}

fn attr_url(base: &Url, e: &quick_xml::events::BytesStart<'_>, key: &str, out: &mut Vec<Url>) {
    for a in e.attributes().flatten() {
        let k = String::from_utf8_lossy(a.key.local_name().as_ref()).to_ascii_lowercase();
        if k == key {
            if let Ok(v) = String::from_utf8(a.value.into_owned()) {
                push_http_url(base, &v, out);
            }
        }
    }
}

fn push_http_url(base: &Url, raw: &str, out: &mut Vec<Url>) {
    if raw.is_empty() || raw.starts_with('#') {
        return;
    }
    if let Ok(u) = base.join(raw) {
        if matches!(u.scheme(), "http" | "https") {
            out.push(u);
            return;
        }
    }
    if let Ok(u) = Url::parse(raw) {
        if matches!(u.scheme(), "http" | "https") {
            out.push(u);
        }
    }
}

fn dedup_urls(mut out: Vec<Url>) -> Vec<Url> {
    out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_enclosure_and_link() {
        let base = Url::parse("https://example.com/").unwrap();
        let xml = r#"<?xml version="1.0"?>
        <rss><channel>
          <item>
            <link>https://example.com/post</link>
            <enclosure url="https://example.com/a.mp3" type="audio/mpeg"/>
          </item>
        </channel></rss>"#;
        let urls = extract_rss_urls(&base, xml);
        assert!(urls
            .iter()
            .any(|u| u.as_str() == "https://example.com/post"));
        assert!(urls
            .iter()
            .any(|u| u.as_str() == "https://example.com/a.mp3"));
    }

    #[test]
    fn atom_link_href() {
        let base = Url::parse("https://example.com/").unwrap();
        let xml = r#"<?xml version="1.0"?>
        <feed xmlns="http://www.w3.org/2005/Atom">
          <entry><link href="https://example.com/e1"/></entry>
        </feed>"#;
        let urls = extract_atom_urls(&base, xml);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].as_str(), "https://example.com/e1");
    }

    #[test]
    fn sitemap_loc() {
        let base = Url::parse("https://example.com/").unwrap();
        let xml = r#"<?xml version="1.0"?>
        <urlset>
          <url><loc>https://example.com/page</loc></url>
        </urlset>"#;
        let urls = extract_sitemap_urls(&base, xml);
        assert_eq!(urls[0].as_str(), "https://example.com/page");
    }
}
