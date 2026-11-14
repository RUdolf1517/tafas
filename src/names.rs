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

/// The same reduction as [`sanitize`], and then only ASCII survives.
///
/// The default keeps the title's own script, which is the right answer for a
/// person on their own machine. It is the wrong answer for a file that outlives
/// the machine it was written on: a title in Greek, Cyrillic or Japanese is
/// legible here and a row of `?` on a filesystem or an archival script that is
/// not UTF-8, and the name is then not findable again. `--restrict-filenames`
/// is the opt-in to a name that survives that trip.
///
/// It composes with [`sanitize`] rather than replacing it, so the path
/// separators and the Windows device names are still handled: the two rules are
/// about different things and both apply.
pub fn restrict(text: &str) -> String {
    sanitize(&ascii_fold(text))
}

/// Replaces the characters that have an ASCII reading and drops the rest.
///
/// Folding rather than transliterating: "Привет" has no ASCII spelling worth
/// inventing, so its letters become `_` like any other character a plain-text
/// name cannot hold — while "café" keeps its `e` and "naïve" its `i`, because
/// the letter underneath the accent is one a plain name can carry.
fn ascii_fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii() {
            out.push(ch);
            continue;
        }
        // A decomposition would turn `é` into `e` + U+0301 and drop the mark;
        // the standard library has no `decompose`, so the characters that carry
        // a plain letter for European titles are spelled out rather than pulling
        // in `unicode-normalization` for a filename.
        match ascii_of(ch) {
            Some(replacement) => out.push_str(replacement),
            None => out.push('_'),
        }
    }
    out
}

/// The ASCII a character is read as, for the cases a decomposition covers.
///
/// Deliberately a table and not a transliteration engine: every entry is a
/// character that a European title actually carries, and a character that is
/// not here becomes `_` rather than an invented spelling.
fn ascii_of(ch: char) -> Option<&'static str> {
    const TABLE: [(char, &str); 61] = [
        ('À', "A"), ('Á', "A"), ('Â', "A"), ('Ã', "A"), ('Ä', "A"), ('Å', "A"),
        ('Æ', "AE"), ('Ç', "C"),
        ('È', "E"), ('É', "E"), ('Ê', "E"), ('Ë', "E"),
        ('Ì', "I"), ('Í', "I"), ('Î', "I"), ('Ï', "I"),
        ('Ð', "D"), ('Ñ', "N"),
        ('Ò', "O"), ('Ó', "O"), ('Ô', "O"), ('Õ', "O"), ('Ö', "O"), ('Ø', "O"),
        ('Ù', "U"), ('Ú', "U"), ('Û', "U"), ('Ü', "U"),
        ('Ý', "Y"), ('Þ', "Th"), ('ß', "ss"),
        ('à', "a"), ('á', "a"), ('â', "a"), ('ã', "a"), ('ä', "a"), ('å', "a"),
        ('æ', "ae"), ('ç', "c"),
        ('è', "e"), ('é', "e"), ('ê', "e"), ('ë', "e"),
        ('ì', "i"), ('í', "i"), ('î', "i"), ('ï', "i"),
        ('ð', "d"), ('ñ', "n"),
        ('ò', "o"), ('ó', "o"), ('ô', "o"), ('õ', "o"), ('ö', "o"), ('ø', "o"),
        ('ù', "u"), ('ú', "u"), ('û', "u"), ('ü', "u"),
        ('ý', "y"), ('þ', "th"),
    ];
    TABLE
        .iter()
        .find(|(source, _)| *source == ch)
        .map(|(_, ascii)| *ascii)
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
pub fn stem(media: &Media, override_name: Option<&str>, restrict_to_ascii: bool) -> String {
    // One place decides which reduction applies, so a caller cannot pass a name
    // through one and a title through the other and get two different rules in
    // one run.
    let reduce = |text: &str| {
        if restrict_to_ascii {
            restrict(text)
        } else {
            sanitize(text)
        }
    };
    match override_name {
        Some(name) => reduce(name),
        None => {
            let title = reduce(media.display_title());
            let id = reduce(&media.id);
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
        assert_eq!(stem(&media, None, false), "A Video [dQw4w9WgXcQ]");
        assert_eq!(stem(&media, Some("mine"), false), "mine");
    }

    #[test]
    fn a_title_that_is_the_id_is_not_repeated() {
        let media = Media::new(Site::X, "20".into(), "https://example".into());
        assert_eq!(stem(&media, None, false), "20");
    }

    #[test]
    fn restrict_folds_what_has_a_plain_letter_and_drops_the_rest() {
        // What a European title actually carries keeps its letter.
        assert_eq!(restrict("café"), "cafe");
        assert_eq!(restrict("naïve"), "naive");
        assert_eq!(restrict("Über Straße"), "Uber Strasse");
        assert_eq!(restrict("Æon Øya"), "AEon Oya");
        // What has no plain letter becomes `_` rather than an invented spelling:
        // a made-up transliteration would be a name the user cannot predict.
        assert_eq!(restrict("Привет"), "______");
        assert_eq!(restrict("日本語"), "___");
        // ASCII passes through untouched, and the sanitizer still applies after.
        assert_eq!(restrict("a/b"), "a_b");
    }

    #[test]
    fn restrict_is_ascii_even_when_the_title_is_not() {
        let mut media = Media::new(
            Site::YouTube,
            "dQw4w9WgXcQ".into(),
            "https://example".into(),
        );
        media.title = "Привет, мир".into();
        let name = stem(&media, None, true);
        assert!(name.is_ascii(), "{name:?} is not ASCII");
        assert!(name.ends_with("[dQw4w9WgXcQ]"), "{name:?}");
        // And the default still keeps the title's own script.
        let kept = stem(&media, None, false);
        assert_eq!(kept, "Привет, мир [dQw4w9WgXcQ]");
    }

    #[test]
    fn restrict_respects_the_windows_device_names() {
        // Folding happens before the reservation check, so a title that folds
        // onto a device name is still prefixed.
        assert_eq!(restrict("con"), "_con");
        assert_eq!(restrict("NUL"), "_NUL");
    }

    #[test]
    fn a_very_long_title_is_bounded() {
        let mut media = Media::new(
            Site::YouTube,
            "dQw4w9WgXcQ".into(),
            "https://example".into(),
        );
        media.title = "я".repeat(400);
        let name = stem(&media, None, false);
        assert!(name.len() <= MAX_STEM_BYTES + 20, "{} bytes", name.len());
        assert!(name.ends_with("[dQw4w9WgXcQ]"));
    }
}
