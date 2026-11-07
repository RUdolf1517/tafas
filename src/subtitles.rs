//! `--write-subs` and `--sub-langs`: the caption tracks, beside the file.
//!
//! A subtitle is written as **WebVTT** and nothing else. That is the format the
//! one site that offers captions serves when asked for it, every player reads
//! it, and converting from it — or to it, from the SRT some other site might
//! hand out one day — is a transcription problem this program does not have.
//!
//! Two rules are enforced here because the alternative is a file that looks
//! fine and is empty: a body that does not begin with `WEBVTT` is a refusal
//! (YouTube's `timedtext` route answers `200` with nothing at all to some
//! networks), and a language selection that matches no track is a refusal that
//! lists the languages the page does have.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::Subtitle;

/// A caption document larger than this is not a caption document.
pub const LIMIT: u64 = 16 * 1024 * 1024;

/// `--sub-langs`: which languages to take.
///
/// `all`, an exact code (`en`, `pt-BR`), or a language with a trailing `*`,
/// which means the language and its regional variants: `en*` is `en`, `en-US`
/// and `en-GB`, but not `eng`. Comparisons ignore case, because a language code
/// is not a password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The text the user typed, kept for messages.
    spec: String,
    all: bool,
    /// `(pattern without the star, is_prefix)`.
    patterns: Vec<(String, bool)>,
}

impl Selection {
    /// `--write-subs` without `--sub-langs`: everything the page offers.
    pub fn all() -> Selection {
        Selection {
            spec: "all".to_string(),
            all: true,
            patterns: Vec::new(),
        }
    }

    pub fn parse(spec: &str) -> Result<Selection> {
        let text = spec.trim();
        if text.is_empty() {
            return Err(Error::Usage(
                "--sub-langs needs a list of languages, such as en,ru or all".to_string(),
            ));
        }

        let mut selection = Selection {
            spec: text.to_string(),
            all: false,
            patterns: Vec::new(),
        };
        for entry in text.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                return Err(Error::Usage(format!(
                    "'{text}' is not a language list: the empty entry between two commas selects nothing"
                )));
            }
            if entry.eq_ignore_ascii_case("all") {
                selection.all = true;
            } else if let Some(prefix) = entry.strip_suffix('*') {
                let prefix = prefix.trim().to_ascii_lowercase();
                if prefix.is_empty() {
                    return Err(Error::Usage(format!(
                        "'{entry}' matches every language; write 'all' if that is what you mean"
                    )));
                }
                selection.patterns.push((prefix, true));
            } else {
                selection.patterns.push((entry.to_ascii_lowercase(), false));
            }
        }
        Ok(selection)
    }

    /// Whether a track written in `language` is selected.
    pub fn matches(&self, language: &str) -> bool {
        if self.all {
            return true;
        }
        let language = language.to_ascii_lowercase();
        self.patterns.iter().any(|(pattern, is_prefix)| {
            if !is_prefix {
                return language == *pattern;
            }
            // `en*` is a language and its variants, not every code that starts
            // with the same three letters: `eng` is not English here.
            language == *pattern
                || language
                    .strip_prefix(pattern.as_str())
                    .is_some_and(|rest| rest.starts_with('-'))
        })
    }
}

impl fmt::Display for Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.spec)
    }
}

/// `<stem>.<language>[.auto].vtt`, next to the media file.
pub fn path(target: &Path, subtitle: &Subtitle) -> PathBuf {
    let stem = target
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "subtitles".to_string());
    let name = subtitle.file_name(&stem);
    match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Fetches one track and writes it beside `target`.
pub fn write(http: &Http, subtitle: &Subtitle, target: &Path) -> Result<PathBuf> {
    let headers: Vec<(&str, &str)> = subtitle
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let bytes = http.get_bytes("subtitles", &subtitle.url, &headers, None, LIMIT)?;
    check_vtt(&bytes, &subtitle.url)?;

    let path = path(target, subtitle);
    let mut temporary = path.clone().into_os_string();
    temporary.push(".part");
    let temporary = PathBuf::from(temporary);

    let written = (|| -> io::Result<()> {
        if let Some(parent) = temporary.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(Error::Io(io::Error::new(
            error.kind(),
            format!("could not write {}: {error}", path.display()),
        )));
    }

    fs::rename(&temporary, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        Error::Io(io::Error::new(
            error.kind(),
            format!("could not put {} in place: {error}", path.display()),
        ))
    })?;
    Ok(path)
}

/// A caption body has to be WebVTT. An empty body is the failure this exists
/// for: `timedtext` answers `200` with zero bytes often enough that treating it
/// as "no subtitles in this language" would be a lie told quietly.
fn check_vtt(bytes: &[u8], url: &str) -> Result<()> {
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let body = body.trim_ascii_start();
    if body.is_empty() {
        return Err(Error::Refused(format!(
            "{url}: the caption route answered with an empty body; YouTube's timedtext does \
             that to some networks, so this is a retry or a different address, not a language \
             the video is missing"
        )));
    }
    if !body.starts_with(b"WEBVTT") {
        return Err(Error::Refused(format!(
            "{url}: the caption route answered {} bytes that are not WebVTT",
            bytes.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subtitle(language: &str, automatic: bool) -> Subtitle {
        Subtitle {
            language: language.to_string(),
            name: None,
            automatic,
            url: format!("https://www.youtube.com/api/timedtext?v=x&lang={language}&fmt=vtt"),
            headers: Vec::new(),
        }
    }

    #[test]
    fn an_exact_code_matches_only_itself() {
        let selection = Selection::parse("en").unwrap();
        assert!(selection.matches("en"));
        assert!(!selection.matches("en-US"), "an exact code is exact");
        assert!(!selection.matches("ru"));
    }

    #[test]
    fn a_star_is_a_language_and_its_variants() {
        let selection = Selection::parse("en*").unwrap();
        assert!(selection.matches("en"));
        assert!(selection.matches("en-US"));
        assert!(selection.matches("en-GB"));
        assert!(!selection.matches("eng"), "a different language with the same prefix");
        assert!(!selection.matches("en_US"), "the separator is the tag's own");
    }

    #[test]
    fn all_is_everything() {
        let selection = Selection::parse("all").unwrap();
        assert!(selection.matches("en"));
        assert!(selection.matches("zh-Hans"));
    }

    #[test]
    fn a_list_is_the_union_and_ignores_case() {
        let selection = Selection::parse("EN, ru ,pt-BR").unwrap();
        assert!(selection.matches("en"));
        assert!(selection.matches("RU"));
        assert!(selection.matches("pt-br"));
        assert!(!selection.matches("de"));
    }

    #[test]
    fn an_empty_or_wild_everything_is_named() {
        assert!(Selection::parse("").is_err());
        assert!(Selection::parse("en,,ru").is_err());
        assert!(Selection::parse("*").is_err(), "a bare star is 'all'");
    }

    #[test]
    fn the_file_name_says_whether_a_person_wrote_it() {
        let target = Path::new("/videos/Big Buck Bunny [aqz-KE-bpKQ].mp4");
        assert_eq!(
            path(target, &subtitle("en", false)),
            PathBuf::from("/videos/Big Buck Bunny [aqz-KE-bpKQ].en.vtt")
        );
        assert_eq!(
            path(target, &subtitle("en", true)),
            PathBuf::from("/videos/Big Buck Bunny [aqz-KE-bpKQ].en.auto.vtt")
        );
    }

    #[test]
    fn a_language_code_cannot_escape_the_directory() {
        let sneaky = Subtitle {
            language: "../../etc/passwd".to_string(),
            name: None,
            automatic: false,
            url: "https://example/captions".to_string(),
            headers: Vec::new(),
        };
        let written = path(Path::new("/videos/clip.mp4"), &sneaky);
        assert_eq!(written.parent(), Some(Path::new("/videos")));
        let name = written.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            name, "clip.------etc-passwd.vtt",
            "separators and dots become dashes, so the name stays one file in one directory"
        );
    }

    #[test]
    fn a_body_that_is_not_webvtt_is_refused() {
        let error = check_vtt(b"", "https://example/captions").unwrap_err();
        assert!(error.to_string().contains("empty body"), "{error}");

        let error = check_vtt(b"{\"error\":true}", "https://example/captions").unwrap_err();
        assert!(error.to_string().contains("not WebVTT"), "{error}");
    }

    #[test]
    fn a_bom_and_leading_whitespace_are_tolerated() {
        assert!(check_vtt(b"\xef\xbb\xbfWEBVTT\n\n", "https://example").is_ok());
        assert!(check_vtt(b"\n  WEBVTT\n", "https://example").is_ok());
    }
}
