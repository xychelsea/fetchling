use std::collections::HashMap;

use url::Url;

#[derive(Debug, Default, Clone)]
pub struct Robots {
    /// user-agent -> disallowed path prefixes
    rules: HashMap<String, Vec<String>>,
    pub sitemaps: Vec<String>,
}

impl Robots {
    pub fn parse(body: &str) -> Self {
        let mut robots = Self::default();
        let mut current_agents: Vec<String> = Vec::new();
        for line in body.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim().to_ascii_lowercase();
                let v = v.trim().to_string();
                match k.as_str() {
                    "user-agent" => {
                        current_agents.push(v.to_ascii_lowercase());
                    }
                    "disallow" => {
                        if current_agents.is_empty() {
                            current_agents.push("*".into());
                        }
                        for a in &current_agents {
                            robots.rules.entry(a.clone()).or_default().push(v.clone());
                        }
                    }
                    "allow" => {}
                    "sitemap" if !v.is_empty() => {
                        robots.sitemaps.push(v);
                    }
                    _ => {}
                }
            }
        }
        robots
    }

    pub fn allows(&self, user_agent: &str, url: &Url) -> bool {
        let ua = user_agent.to_ascii_lowercase();
        let path = url.path();
        let prefixes = self
            .rules
            .get(&ua)
            .or_else(|| self.rules.get("*"))
            .cloned()
            .unwrap_or_default();
        for p in prefixes {
            if p.is_empty() {
                continue;
            }
            if path.starts_with(&p) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disallow_root() {
        let r = Robots::parse("User-agent: *\nDisallow: /private\n");
        let u = Url::parse("http://ex/private/a").unwrap();
        assert!(!r.allows("fetchling", &u));
        let u2 = Url::parse("http://ex/public").unwrap();
        assert!(r.allows("fetchling", &u2));
    }

    #[test]
    fn collects_sitemap_lines() {
        let r = Robots::parse(
            "User-agent: *\nDisallow:\nSitemap: https://example.com/sitemap.xml\nSitemap: https://example.com/other.xml\n",
        );
        assert_eq!(r.sitemaps.len(), 2);
        assert_eq!(r.sitemaps[0], "https://example.com/sitemap.xml");
    }
}
