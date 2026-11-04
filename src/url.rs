use crate::error::{Error, Result};

/// A URL split into the four parts tafas actually needs.
///
/// This is not a general-purpose URL parser: it does not resolve relative
/// references, normalise dot segments or decode percent escapes beyond the one
/// place where a path segment has to be read (a YouTube id, which is
/// `[A-Za-z0-9_-]{11}` and never encoded). Keeping it small keeps the one
/// property that matters: a URL that does not parse is rejected before any
/// request leaves the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    /// Lowercased, without a trailing dot. A port, if present, is kept here.
    pub authority: String,
    /// Always starts with `/`.
    pub path: String,
    /// Without the leading `?`.
    pub query: String,
}

impl Url {
    pub fn parse(raw: &str) -> Result<Url> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(Error::Usage("empty URL".into()));
        }

        let (scheme, rest) = match raw.find("://") {
            Some(i) => (&raw[..i], &raw[i + 3..]),
            None => ("https", raw),
        };
        let scheme_lower = scheme.to_ascii_lowercase();
        if scheme_lower != "http" && scheme_lower != "https" {
            return Err(Error::Usage(format!(
                "unsupported URL scheme '{scheme}': tafas speaks http and https"
            )));
        }

        // A query or fragment can never appear before the path, so splitting on
        // the first '/' is enough to find the authority.
        let (authority, tail) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(Error::Usage(format!("URL has no host: {raw}")));
        }

        // Userinfo is accepted and dropped rather than rejected: it is never
        // used by the three supported sites, and pretending to honour it would
        // put a credential into a log line.
        let authority = match authority.rfind('@') {
            Some(i) => &authority[i + 1..],
            None => authority,
        };

        let (before_fragment, _fragment) = match tail.find('#') {
            Some(i) => (&tail[..i], &tail[i..]),
            None => (tail, ""),
        };
        let (path, query) = match before_fragment.find('?') {
            Some(i) => (&before_fragment[..i], &before_fragment[i + 1..]),
            None => (before_fragment, ""),
        };

        Ok(Url {
            scheme: scheme_lower,
            authority: authority.to_ascii_lowercase(),
            path: if path.is_empty() { "/".into() } else { path.into() },
            query: query.into(),
        })
    }

    /// The host without `www.` or a port, lowercased. `None` for an authority
    /// this parser cannot name (an IPv6 literal, an empty host).
    pub fn host(&self) -> Option<&str> {
        let host = match self.authority.rfind(':') {
            // A colon before a non-numeric tail is not a port; leave it alone.
            Some(i) if self.authority[i + 1..].chars().all(|c| c.is_ascii_digit()) => {
                &self.authority[..i]
            }
            _ => &self.authority[..],
        };
        let host = host.strip_suffix('.').unwrap_or(host);
        let host = host.strip_prefix("www.").unwrap_or(host);
        if host.is_empty() {
            None
        } else {
            Some(host)
        }
    }

    /// Path segments, empty ones removed: `/a//b/` is `["a", "b"]`.
    pub fn segments(&self) -> Vec<&str> {
        self.path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    }

    pub fn query_param(&self, name: &str) -> Option<String> {
        for pair in self.query.split('&') {
            let (key, value) = match pair.find('=') {
                Some(i) => (&pair[..i], &pair[i + 1..]),
                None => (pair, ""),
            };
            if key == name {
                return Some(percent_decode(value));
            }
        }
        None
    }
}

/// Decodes `%XX` and `+`. Deliberately total: a malformed escape is passed
/// through unchanged, because a wrong id is caught by the id check that
/// follows, while an error here would reject a page URL for no reason.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi * 16 + lo);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_page_url() {
        let u = Url::parse("https://WWW.YouTube.com./watch?v=abc&t=10").unwrap();
        assert_eq!(u.host(), Some("youtube.com"));
        assert_eq!(u.path, "/watch");
        assert_eq!(u.query_param("v").as_deref(), Some("abc"));
        assert_eq!(u.query_param("t").as_deref(), Some("10"));
        assert_eq!(u.query_param("missing"), None);
    }

    #[test]
    fn keeps_the_port_out_of_the_host() {
        let u = Url::parse("http://example.com:8080/a").unwrap();
        assert_eq!(u.host(), Some("example.com"));
        assert_eq!(u.segments(), vec!["a"]);
    }

    #[test]
    fn rejects_other_schemes() {
        assert!(Url::parse("ftp://example.com/x").unwrap_err().is_usage());
        assert!(Url::parse("javascript:alert(1)").unwrap_err().is_usage());
        assert!(Url::parse("https://").unwrap_err().is_usage());
    }

    #[test]
    fn drops_fragment_and_userinfo() {
        let u = Url::parse("https://user:pw@x.com/p#frag").unwrap();
        assert_eq!(u.host(), Some("x.com"));
        assert_eq!(u.path, "/p");
        assert_eq!(u.query, "");
    }

    #[test]
    fn decodes_query_escapes() {
        let u = Url::parse("https://x.com/s?q=a%20b+c").unwrap();
        assert_eq!(u.query_param("q").as_deref(), Some("a b c"));
    }
}
