//! A Netscape cookie file, read once, for the pages that hide behind a session.
//!
//! The format is the one curl writes and every browser's "export cookies"
//! button produces: seven tab-separated fields — domain, include-subdomains,
//! path, secure, expiry, name, value — with `#HttpOnly_` lines that are cookies
//! rather than comments.
//!
//! Three rules, and they are the whole design:
//!
//! * the file is read **once**, at startup, and never written back. A run that
//!   cannot write cannot corrupt the jar a browser is still using;
//! * a cookie is sent only where it applies — host, path, `Secure`, expiry are
//!   all checked, so a jar full of browser cookies does not leak a session
//!   cookie to a CDN that merely shares a parent domain;
//! * no cookie value is ever printed, logged or quoted in an error. `--verbose`
//!   logs the request, not its headers, and this module's `Debug` prints a
//!   count.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::url::Url;

/// A cookie file larger than this is not a cookie file.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// How many cookies are read before the rest is ignored: a jar with more than
/// this is a browser profile, not a session.
const MAX_COOKIES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cookie {
    /// Lowercased, without a leading dot.
    domain: String,
    include_subdomains: bool,
    path: String,
    secure: bool,
    /// Seconds since the epoch; `0` means "until the program exits".
    expires: u64,
    name: String,
    value: String,
}

impl Cookie {
    fn applies(&self, host: &str, path: &str, https: bool, now: u64) -> bool {
        if self.secure && !https {
            return false;
        }
        if self.expires != 0 && self.expires <= now {
            return false;
        }
        let domain_matches = if self.include_subdomains {
            host == self.domain
                || host
                    .strip_suffix(&self.domain)
                    .map(|prefix| prefix.ends_with('.'))
                    .unwrap_or(false)
        } else {
            host == self.domain
        };
        domain_matches && path_matches(&self.path, path)
    }
}

/// The cookies from one file, and the one question worth asking them.
#[derive(Clone, Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
}

impl Jar {
    /// Reads `path`. A file that cannot be read is an I/O failure; a file that
    /// contains no cookie at all is a usage error, because it is almost always
    /// a JSON export or a `wget` log rather than the thing that was meant.
    pub fn load(path: &Path) -> Result<Jar> {
        let metadata = fs::metadata(path).map_err(|error| {
            Error::Io(io::Error::new(
                error.kind(),
                format!("could not read {}: {error}", path.display()),
            ))
        })?;
        if metadata.is_dir() {
            return Err(Error::Usage(format!(
                "{} is a directory, not a cookie file",
                path.display()
            )));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(Error::Limit(format!(
                "{} is larger than {MAX_FILE_BYTES} bytes and is not a cookie file",
                path.display()
            )));
        }
        let text = fs::read_to_string(path).map_err(|error| {
            Error::Io(io::Error::new(
                error.kind(),
                format!("could not read {}: {error}", path.display()),
            ))
        })?;
        let jar = Jar::parse(&text);
        if jar.cookies.is_empty() {
            return Err(Error::Usage(format!(
                "{}: no cookies in Netscape format were found (seven tab-separated fields per \
                 line; a JSON export is not one)",
                path.display()
            )));
        }
        Ok(jar)
    }

    /// Reads the text. Lines that are not cookies are skipped rather than
    /// refused: browsers write comments and blank lines, and a jar with one odd
    /// line is still the jar the user meant.
    pub fn parse(text: &str) -> Jar {
        let mut cookies = Vec::new();
        for line in text.lines() {
            if cookies.len() >= MAX_COOKIES {
                break;
            }
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
            if line.trim_start().starts_with('#') {
                continue;
            }
            if let Some(cookie) = parse_line(line) {
                cookies.push(cookie);
            }
        }
        Jar { cookies }
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// The `Cookie:` header for a request to `url`, or `None` when no cookie
    /// applies. Cookies are sent longest-path-first, which is the order a
    /// server resolving duplicate names expects.
    pub fn header(&self, url: &Url) -> Option<String> {
        let host = host_of(url);
        if host.is_empty() {
            return None;
        }
        let https = url.scheme.eq_ignore_ascii_case("https");
        let now = now_seconds();
        let mut matching: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|cookie| cookie.applies(&host, &url.path, https, now))
            .collect();
        if matching.is_empty() {
            return None;
        }
        matching.sort_by_key(|cookie| std::cmp::Reverse(cookie.path.len()));
        let pairs: Vec<String> = matching
            .iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect();
        Some(pairs.join("; "))
    }
}

impl fmt::Debug for Jar {
    /// Deliberately without the values: a `Debug` that prints a session cookie
    /// is a `Debug` that prints it in a panic message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Jar")
            .field("cookies", &self.cookies.len())
            .finish()
    }
}

fn parse_line(line: &str) -> Option<Cookie> {
    let mut fields = line.split('\t');
    let domain = fields.next()?;
    let include_subdomains = fields.next()?;
    let path = fields.next()?;
    let secure = fields.next()?;
    let expires = fields.next()?;
    let name = fields.next()?;
    let value = fields.next()?;
    if fields.next().is_some() {
        // Eight fields means a value with a tab in it, which the format cannot
        // represent; guessing which tab was the separator is worse than
        // dropping the line.
        return None;
    }
    if name.is_empty() {
        return None;
    }
    let dotted = domain.trim().starts_with('.');
    let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        return None;
    }
    Some(Cookie {
        domain,
        include_subdomains: include_subdomains.trim().eq_ignore_ascii_case("TRUE") || dotted,
        path: {
            let path = path.trim();
            if path.starts_with('/') {
                path.to_string()
            } else {
                format!("/{path}")
            }
        },
        secure: secure.trim().eq_ignore_ascii_case("TRUE"),
        expires: expires.trim().parse().unwrap_or(0),
        name: name.to_string(),
        value: value.to_string(),
    })
}

/// The host a cookie is matched against: lowercased, without the port and
/// without a trailing dot. `www.` is **kept** — unlike `Url::host`, which drops
/// it for site detection, a cookie scoped to `www.example.com` must not travel
/// to `example.com`.
fn host_of(url: &Url) -> String {
    let authority = url.authority.as_str();
    let host = match authority.rfind(':') {
        Some(index)
            if !authority[index + 1..].is_empty()
                && authority[index + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            &authority[..index]
        }
        _ => authority,
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// The path rule from RFC 6265 §5.1.4: an exact match, or a prefix that ends at
/// a `/` boundary — `/foo` matches `/foo` and `/foo/bar` but not `/foobar`.
fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    if cookie_path == "/" || cookie_path == request_path {
        return true;
    }
    if let Some(rest) = request_path.strip_prefix(cookie_path) {
        return cookie_path.ends_with('/') || rest.starts_with('/');
    }
    false
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    const FILE: &str = "\
# Netscape HTTP Cookie File
# https://curl.se/docs/http-cookies.html
.example.com\tTRUE\t/\tFALSE\t0\tSID\tabc123
#HttpOnly_.example.com\tTRUE\t/private\tTRUE\t0\tTOKEN\tsecret
other.test\tFALSE\t/\tFALSE\t0\tNAME\tvalue
expired.test\tFALSE\t/\tFALSE\t1\tOLD\tgone
";

    #[test]
    fn reads_the_netscape_format_including_the_http_only_prefix() {
        let jar = Jar::parse(FILE);
        assert_eq!(jar.len(), 4, "comments and blank lines are skipped");
        assert!(!jar.is_empty());
    }

    #[test]
    fn a_cookie_goes_to_its_domain_and_its_prefixes() {
        let jar = Jar::parse(FILE);
        assert_eq!(
            jar.header(&url("https://www.example.com/watch?v=1"))
                .as_deref(),
            Some("SID=abc123")
        );
        assert_eq!(
            jar.header(&url("https://example.com/")).as_deref(),
            Some("SID=abc123")
        );
        assert_eq!(
            jar.header(&url("https://api.example.com/")).as_deref(),
            Some("SID=abc123"),
            "a leading dot includes subdomains"
        );
    }

    #[test]
    fn another_host_gets_nothing() {
        let jar = Jar::parse(FILE);
        assert_eq!(jar.header(&url("https://evil.test/")), None);
        assert_eq!(
            jar.header(&url("https://notexample.com/")),
            None,
            "a suffix match must land on a dot boundary"
        );
    }

    #[test]
    fn a_secure_cookie_is_not_sent_over_plain_http() {
        let jar = Jar::parse(FILE);
        assert_eq!(
            jar.header(&url("https://example.com/private/thing"))
                .as_deref(),
            Some("TOKEN=secret; SID=abc123"),
            "the longest path comes first"
        );
        assert_eq!(
            jar.header(&url("http://example.com/private/thing"))
                .as_deref(),
            Some("SID=abc123"),
            "Secure is honoured"
        );
    }

    #[test]
    fn a_path_prefix_stops_at_a_segment_boundary() {
        let jar = Jar::parse(FILE);
        assert_eq!(
            jar.header(&url("https://example.com/privateer")),
            Some("SID=abc123".to_string())
        );
    }

    #[test]
    fn an_expired_cookie_is_dropped() {
        let jar = Jar::parse("expired.test\tFALSE\t/\tFALSE\t1\tOLD\tgone\n");
        assert_eq!(jar.len(), 1);
        assert_eq!(jar.header(&url("https://expired.test/")), None);
    }

    #[test]
    fn a_session_cookie_never_expires() {
        let jar = Jar::parse("session.test\tFALSE\t/\tFALSE\t0\tS\tv\n");
        assert_eq!(
            jar.header(&url("https://session.test/")).as_deref(),
            Some("S=v")
        );
    }

    #[test]
    fn a_line_with_the_wrong_number_of_fields_is_skipped() {
        let jar = Jar::parse("too\tfew\tfields\n\nalso\tFALSE\t/\tFALSE\t0\tN\tV\textra\n");
        assert!(jar.is_empty());
    }

    #[test]
    fn a_file_with_no_cookies_is_a_usage_error() {
        let path = std::env::temp_dir().join(format!("tafas-cookies-{}", std::process::id()));
        std::fs::write(&path, "{\"cookies\": []}\n").unwrap();
        let error = Jar::load(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(
            error.to_string().contains("Netscape"),
            "the message should name the format: {error}"
        );
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let error = Jar::load(Path::new("/nonexistent/tafas/cookies.txt")).unwrap_err();
        assert!(matches!(error, Error::Io(_)), "{error}");
    }

    #[test]
    fn debug_prints_a_count_not_a_value() {
        let jar = Jar::parse(FILE);
        let text = format!("{jar:?}");
        assert!(text.contains("cookies: 4"), "{text}");
        assert!(
            !text.contains("abc123"),
            "values must not be printable: {text}"
        );
    }
}
