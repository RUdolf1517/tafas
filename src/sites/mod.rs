//! One reader per site, and the table that holds them.
//!
//! Every extractor has the same contract: take a page URL, return [`Media`]
//! with the streams it can actually fetch, or fail with a reason a user can act
//! on. Nothing here downloads media; that is `download.rs`. Nothing here guesses
//! at a value it could not parse: a title that cannot be read becomes the id, a
//! format whose URL is cipher-protected is dropped, and a page that answers with
//! a consent wall is a refusal rather than an empty success.

// Each site is behind its feature: a build without `twitch` contains no Twitch
// code at all, not a route that refuses. The list reader belongs to YouTube
// because the only lists this build reads are YouTube's.
#[cfg(feature = "reddit")]
pub mod reddit;
#[cfg(feature = "soundcloud")]
pub mod soundcloud;
#[cfg(feature = "tiktok")]
pub mod tiktok;
#[cfg(feature = "twitch")]
pub mod twitch;
#[cfg(feature = "vimeo")]
pub mod vimeo;
#[cfg(feature = "x")]
pub mod x;
#[cfg(feature = "youtube")]
pub mod youtube;
#[cfg(feature = "youtube")]
pub mod youtube_list;

use serde_json::Value;

pub use crate::model::{Listing, Media};

pub mod registry;

pub use registry::{describe_build, extract, for_host, names, Reader, Target, READERS};

/// The site names this build reads. Derived from the reader table rather than
/// written down twice, which is what keeps `--capabilities` honest in a build
/// that compiles a site out.
pub fn sites() -> Vec<&'static str> {
    names()
}

// --- small JSON helpers ----------------------------------------------------
//
// The extractors read documents whose shape is not theirs to define, so every
// lookup is optional and the *caller* decides what a missing field means. A
// `serde` derive would turn a missing key into a parse failure for the whole
// page, which is exactly the brittleness this tool has to avoid.

pub(crate) fn at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    Some(current)
}

pub(crate) fn text(value: &Value, path: &[&str]) -> Option<String> {
    at(value, path)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(crate) fn uint(value: &Value, path: &[&str]) -> Option<u64> {
    match at(value, path)? {
        Value::Number(n) => n.as_u64(),
        // YouTube quotes `contentLength` and `lengthSeconds` as strings, so a
        // numeric string has to be accepted without pretending that everything
        // on the internet is a number.
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

pub(crate) fn list<'a>(value: &'a Value, path: &[&str]) -> &'a [Value] {
    at(value, path)
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// Extracts the balanced JSON object that follows `marker` in an HTML page.
///
/// Sites inline their state as `var x = {...};`, so the object has to be found
/// by brace matching that understands strings and escapes — a `}` inside a
/// title would otherwise truncate the document.
pub fn json_after<'a>(page: &'a str, marker: &str) -> Option<&'a str> {
    let start = page.find(marker)?;
    let tail = &page[start + marker.len()..];
    let open = tail.find('{')?;
    let bytes = tail.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return tail.get(open..=index);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_after_survives_braces_in_strings() {
        let page = r#"<script>var ytInitialPlayerResponse = {"a":"}{","b":{"c":1}};</script>"#;
        let found = json_after(page, "ytInitialPlayerResponse = ").unwrap();
        let value: Value = serde_json::from_str(found).unwrap();
        assert_eq!(value["a"], "}{");
        assert_eq!(value["b"]["c"], 1);
    }

    #[test]
    fn json_after_survives_escaped_quotes() {
        let page = r#"x = {"a":"he said \"}\"","b":2}"#;
        let found = json_after(page, "x = ").unwrap();
        let value: Value = serde_json::from_str(found).unwrap();
        assert_eq!(value["b"], 2);
    }

    #[test]
    fn json_after_reports_absence() {
        assert!(json_after("nothing here", "x = ").is_none());
        assert!(json_after("x = {", "x = ").is_none());
    }

    #[test]
    fn helpers_accept_quoted_numbers_and_skip_empty_text() {
        let value = json!({
            "n": "1234",
            "m": 12,
            "s": "  ",
            "t": " hello ",
            "list": [1, 2]
        });
        assert_eq!(uint(&value, &["n"]), Some(1234));
        assert_eq!(uint(&value, &["m"]), Some(12));
        assert_eq!(uint(&value, &["missing"]), None);
        assert_eq!(text(&value, &["s"]), None);
        assert_eq!(text(&value, &["t"]).as_deref(), Some("hello"));
        assert_eq!(list(&value, &["list"]).len(), 2);
        assert!(list(&value, &["missing"]).is_empty());
    }
}
