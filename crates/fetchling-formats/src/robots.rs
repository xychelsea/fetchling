use std::collections::HashMap;

use url::Url;

/// robots.txt subset: `User-agent`, `Disallow`, and `Sitemap`.
///
/// `Allow` rules are ignored. Comments (`#`) are stripped.
#[derive(Debug, Default, Clone)]
pub struct Robots {
    /// user-agent -> disallowed path prefixes
    rules: HashMap<String, Vec<String>>,
    /// `Sitemap:` values collected while parsing.
    pub sitemaps: Vec<String>,
}

impl Robots {
    /// Disallow every path for every user-agent.
    pub fn deny_all() -> Self {
        let mut robots = Self::default();
        robots.rules.insert("*".into(), vec!["/".into()]);
        robots
    }

    /// Parse a robots.txt body.
    ///
    /// `Disallow` prefixes apply to the current `User-agent` list (`*` if none
    /// was seen). Empty `Disallow` values are stored but ignored by
    /// [`Self::allows`].
    ///
    /// # Examples
    ///
    /// ```
    /// use fetchling_formats::Robots;
    /// use url::Url;
    ///
    /// let r = Robots::parse("User-agent: *\nDisallow: /private\n");
    /// assert!(!r.allows("fetchling", &Url::parse("http://ex/private/a").unwrap()));
    /// assert!(r.allows("fetchling", &Url::parse("http://ex/public").unwrap()));
    /// ```
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

    /// Whether `user_agent` may fetch `url`'s path.
    ///
    /// Uses the agent-specific `Disallow` list, or `*` when that agent has no
    /// rules. A path is denied when it starts with a stored prefix.
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
    fn deny_all_blocks_everything() {
        let r = Robots::deny_all();
        let u = Url::parse("http://ex/anything").unwrap();
        assert!(!r.allows("fetchling", &u));
    }

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

    #[test]
    fn parse_comments_empty_disallow_and_allow() {
        let r = Robots::parse(
            "# Disallow: /\nUser-agent: *\nDisallow:\nAllow: /private\nDisallow: /private\n",
        );
        assert!(r.allows("fetchling", &Url::parse("http://ex/").unwrap()));
        assert!(!r.allows("fetchling", &Url::parse("http://ex/private").unwrap()));
        let r = Robots::parse("Disallow: /hidden\n");
        assert!(!r.allows("fetchling", &Url::parse("http://ex/hidden").unwrap()));
        assert!(r.allows("fetchling", &Url::parse("http://ex/ok").unwrap()));
    }
}
