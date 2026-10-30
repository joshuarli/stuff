use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Route {
    pub prefix: String,
    pub site_dir: PathBuf,
    pub canonical_site_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub routes: Vec<Route>,
}

impl Config {
    pub fn single(site_dir: PathBuf) -> std::io::Result<Self> {
        Ok(Self {
            routes: vec![Route {
                prefix: String::new(),
                canonical_site_dir: std::fs::canonicalize(&site_dir)?,
                site_dir,
            }],
        })
    }

    pub fn match_route(&self, path: &str) -> Option<(&Route, String)> {
        let mut best: Option<(&Route, String)> = None;
        for route in &self.routes {
            let rest = if route.prefix.is_empty() {
                path.to_string()
            } else {
                let with_slash = format!("/{}/", route.prefix.trim_matches('/'));
                if path == with_slash.trim_end_matches('/') {
                    "/".to_string()
                } else if let Some(rest) = path.strip_prefix(&with_slash) {
                    format!("/{rest}")
                } else {
                    continue;
                }
            };
            let replace = match &best {
                None => true,
                Some((r, _)) => r.prefix.is_empty() || r.prefix.len() < route.prefix.len(),
            };
            if replace {
                best = Some((route, rest));
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            routes: vec![
                Route {
                    prefix: String::new(),
                    site_dir: PathBuf::from("/srv/a"),
                    canonical_site_dir: PathBuf::from("/srv/a"),
                },
                Route {
                    prefix: "mirror".to_string(),
                    site_dir: PathBuf::from("/srv/b"),
                    canonical_site_dir: PathBuf::from("/srv/b"),
                },
            ],
        }
    }

    #[test]
    fn longest_prefix_wins() {
        let c = cfg();
        let (r, rest) = c.match_route("/mirror/current/x").unwrap();
        assert_eq!(r.prefix, "mirror");
        assert_eq!(rest, "/current/x");
        let (r, rest) = c.match_route("/other/y").unwrap();
        assert_eq!(r.prefix, "");
        assert_eq!(rest, "/other/y");
        let (r, rest) = c.match_route("/mirror").unwrap();
        assert_eq!(r.prefix, "mirror");
        assert_eq!(rest, "/");
    }
}
