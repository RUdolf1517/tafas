//! Output names.
//!
//! A title is attacker-controlled text that becomes a path, so it is reduced to
//! something a filesystem accepts before it is used: path separators, control
//! characters and the characters Windows reserves become `_`, runs of
//! whitespace collapse, trailing dots and spaces are dropped (Windows strips
//! them silently, which would make the written name differ from the reported
//! one), and the result is bounded in *bytes* at a character boundary.
//!
//! Nothing here invents a name from nothing: an empty title falls back to the
//! media id, which is already `[A-Za-z0-9_-]`.

use crate::model::Media;

/// Long enough for any real title, short enough for every filesystem in
/// practice once the extension and the ` [id]` suffix are added.
pub const MAX_STEM_BYTES: usize = 150;

/// Reserved device names on Windows, case-insensitively, with or without an
/// extension.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_STEM_BYTES));
    let mut pending_space = false;

    for ch in text.chars() {
        if ch.is_whitespace() || ch.is_control() {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.push(match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        });
    }

    let mut cleaned = out
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(' ')
        .to_string();
    cleaned = truncate(cleaned, MAX_STEM_BYTES);

    if cleaned.is_empty() {
        return "untitled".to_string();
    }
    if RESERVED.contains(&cleaned.to_ascii_lowercase().as_str()) {
        // A file called `nul` cannot be created on Windows; prefixing keeps the
        // name recognisable instead of dropping the title entirely.
        cleaned.insert(0, '_');
    }
    cleaned
}

/// Truncates at a character boundary, never mid-codepoint.
pub fn truncate(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim_end().to_string()
}

/// The name a file is given, without an extension.
///
/// The id is part of the default stem on purpose: two videos called "Live"
/// would otherwise overwrite each other, and the id is the only stable handle a
/// user has for finding the page again.
pub fn stem(media: &Media, override_name: Option<&str>) -> String {
    match override_name {
        Some(name) => sanitize(name),
        None => {
            let title = sanitize(media.display_title());
            let id = sanitize(&media.id);
            if id.is_empty() || id == title {
                title
            } else {
                let room = MAX_STEM_BYTES.saturating_sub(id.len() + 3);
                format!("{} [{id}]", truncate(title, room))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Media, Site};

    #[test]
    fn removes_what_a_filesystem_refuses() {
        assert_eq!(sanitize("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize("line\nbreak"), "line break");
        assert_eq!(sanitize("tab\there"), "tab here");
    }

    #[test]
    fn collapses_and_trims_whitespace() {
        assert_eq!(sanitize("  a   b  "), "a b");
        assert_eq!(sanitize("ends with dots..."), "ends with dots");
    }

    #[test]
    fn keeps_non_ascii_intact() {
        assert_eq!(sanitize("Привет, мир"), "Привет, мир");
        assert_eq!(sanitize("日本語のタイトル"), "日本語のタイトル");
    }

    #[test]
    fn truncates_at_a_character_boundary() {
        let long = "ж".repeat(200);
        let cut = truncate(long, 10);
        assert_eq!(cut.len(), 10);
        assert!(cut.chars().all(|c| c == 'ж'));
    }

    #[test]
    fn an_empty_title_becomes_a_placeholder() {
        assert_eq!(sanitize("   "), "untitled");
        assert_eq!(sanitize("..."), "untitled");
    }

    #[test]
    fn windows_device_names_are_prefixed() {
        assert_eq!(sanitize("nul"), "_nul");
        assert_eq!(sanitize("CON"), "_CON");
    }

    #[test]
    fn the_default_stem_carries_the_id() {
        let mut media = Media::new(
            Site::YouTube,
            "dQw4w9WgXcQ".into(),
            "https://example".into(),
        );
        media.title = "A Video".into();
        assert_eq!(stem(&media, None), "A Video [dQw4w9WgXcQ]");
        assert_eq!(stem(&media, Some("mine")), "mine");
    }

    #[test]
    fn a_title_that_is_the_id_is_not_repeated() {
        let media = Media::new(Site::X, "20".into(), "https://example".into());
        assert_eq!(stem(&media, None), "20");
    }

    #[test]
    fn a_very_long_title_is_bounded() {
        let mut media = Media::new(
            Site::YouTube,
            "dQw4w9WgXcQ".into(),
            "https://example".into(),
        );
        media.title = "я".repeat(400);
        let name = stem(&media, None);
        assert!(name.len() <= MAX_STEM_BYTES + 20, "{} bytes", name.len());
        assert!(name.ends_with("[dQw4w9WgXcQ]"));
    }
}
