//! `--write-subs`, `--embed-subs` and `--sub-langs`: the caption tracks.
//!
//! A track becomes a file beside the video, or one more track inside it. Both
//! paths start from the same WebVTT body and the same cue parse; only the last
//! step differs, which is why they live in one module.
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
use crate::ts::{Codec, Sample, Track, TrackKind};

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

/// Fetches one track's WebVTT body, which both the sidecar and the embedded
/// track start from.
pub fn body(http: &Http, subtitle: &Subtitle) -> Result<Vec<u8>> {
    let headers: Vec<(&str, &str)> = subtitle
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let bytes = http.get_bytes("subtitles", &subtitle.url, &headers, None, LIMIT)?;
    check_vtt(&bytes, &subtitle.url)?;
    Ok(bytes)
}

/// Fetches one track and writes it beside `target`.
pub fn write(http: &Http, subtitle: &Subtitle, target: &Path) -> Result<PathBuf> {
    let bytes = body(http, subtitle)?;

    let path = path(target, subtitle);
    let mut temporary = path.clone().into_os_string();
    temporary.push(".part");
    let temporary = PathBuf::from(temporary);

    let written = (|| -> io::Result<()> {
        if let Some(parent) = temporary.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(crate::paths::verbatim(parent))?;
            }
        }
        let mut file = fs::File::create(crate::paths::verbatim(&temporary))?;
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

    fs::rename(crate::paths::verbatim(&temporary), crate::paths::verbatim(&path)).map_err(|error| {
        let _ = fs::remove_file(crate::paths::verbatim(&temporary));
        Error::Io(io::Error::new(
            error.kind(),
            format!("could not put {} in place: {error}", path.display()),
        ))
    })?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// the same track, inside the container
// ---------------------------------------------------------------------------

/// One cue: when it appears, when it goes, and the text in between.
///
/// Times are milliseconds because that is the unit a WebVTT body states, and
/// the track is written at a timescale of 1000 for the same reason: a cue time
/// is not a picture, and rounding it into a 90 kHz clock buys nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Parses a WebVTT body into cues.
///
/// What is kept is what a player needs: the times and the text. Cue settings
/// (`line:`, `align:`, `size:`) and the `STYLE`/`REGION` blocks are dropped,
/// because the container's own cue boxes cannot carry them without a second
/// parse and the position a cue lands in is the player's decision anyway. That
/// is a documented limitation, not a silent one: it is in `docs/cli.md`.
pub fn cues(bytes: &[u8]) -> Result<Vec<Cue>> {
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = std::str::from_utf8(body)
        .map_err(|_| Error::Parse("the caption body is not UTF-8".to_string()))?;
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut blocks = text.split("\n\n");
    let header = blocks
        .next()
        .ok_or_else(|| Error::Parse("the caption body is empty".to_string()))?;
    if !header.trim_start().starts_with("WEBVTT") {
        return Err(Error::Parse(
            "the caption body does not start with WEBVTT".to_string(),
        ));
    }

    let mut cues: Vec<Cue> = Vec::new();
    for block in blocks {
        let lines: Vec<&str> = block
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.is_empty() {
            continue;
        }
        let first = lines[0].trim_start();
        if first.starts_with("NOTE") || first.starts_with("STYLE") || first.starts_with("REGION") {
            continue;
        }
        let timing_index = if lines[0].contains("-->") { 0 } else { 1 };
        let timing = match lines.get(timing_index) {
            Some(line) if line.contains("-->") => *line,
            // A block with neither a timing line nor a comment is not a cue;
            // refusing beats guessing what it was meant to be.
            _ => {
                return Err(Error::Parse(format!(
                    "a cue block has no --> timing line: {:?}",
                    lines[0]
                )))
            }
        };
        let (start_ms, end_ms) = timing_pair(timing)?;
        let text = lines[timing_index + 1..].join("\n");
        if text.trim().is_empty() {
            continue;
        }
        cues.push(Cue {
            start_ms,
            end_ms,
            text,
        });
    }

    // `stts` is a table of non-decreasing decode times, so the cues are put in
    // order here rather than trusting a body that may overlap or be unsorted.
    cues.sort_by_key(|cue| (cue.start_ms, cue.end_ms));
    for cue in &cues {
        if cue.end_ms <= cue.start_ms {
            return Err(Error::Parse(format!(
                "a cue from {} ms ends at {} ms, so it has no duration",
                cue.start_ms, cue.end_ms
            )));
        }
    }
    Ok(cues)
}

/// `00:00:01.500 --> 00:00:03.000` or `01:30.000 --> 01:32.250`, with any
/// settings after the second time ignored.
fn timing_pair(line: &str) -> Result<(u64, u64)> {
    let (left, right) = line
        .split_once("-->")
        .ok_or_else(|| Error::Parse(format!("a cue timing line has no -->: {line:?}")))?;
    let start = timestamp(left.trim())?;
    // The end time is followed by optional cue settings.
    let end_text = right.split_whitespace().next().unwrap_or("");
    let end = timestamp(end_text)?;
    Ok((start, end))
}

fn timestamp(text: &str) -> Result<u64> {
    let mut parts: Vec<&str> = text.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return Err(Error::Parse(format!("{text:?} is not a WebVTT timestamp")));
    }
    let seconds_part = parts.pop().unwrap_or("");
    let (seconds, millis) = match seconds_part.split_once('.') {
        Some((seconds, millis)) => (seconds, Some(millis)),
        None => match seconds_part.split_once(',') {
            Some((seconds, millis)) => (seconds, Some(millis)),
            None => (seconds_part, None),
        },
    };
    let seconds: u64 = seconds
        .parse()
        .map_err(|_| Error::Parse(format!("{text:?} is not a WebVTT timestamp")))?;
    let millis: u64 = match millis {
        Some(millis) if millis.len() == 3 => millis
            .parse()
            .map_err(|_| Error::Parse(format!("{text:?} is not a WebVTT timestamp")))?,
        // A fraction that is not exactly three digits would need rounding, and
        // a rounding nobody can see is worse than a refusal.
        Some(millis) => {
            return Err(Error::Parse(format!(
                "{text:?} has a fraction of {} digits; WebVTT states milliseconds",
                millis.len()
            )))
        }
        None => 0,
    };
    if seconds >= 60 {
        return Err(Error::Parse(format!(
            "{text:?} has {seconds} seconds in its seconds field"
        )));
    }
    let mut minutes: u64 = 0;
    if let Some(minutes_part) = parts.pop() {
        minutes = minutes_part
            .parse()
            .map_err(|_| Error::Parse(format!("{text:?} is not a WebVTT timestamp")))?;
    }
    let mut hours: u64 = 0;
    if let Some(hours_part) = parts.pop() {
        hours = hours_part
            .parse()
            .map_err(|_| Error::Parse(format!("{text:?} is not a WebVTT timestamp")))?;
        // With hours present, the minute field exists and is bounded.
        if minutes >= 60 {
            return Err(Error::Parse(format!(
                "{text:?} has {minutes} minutes in its minutes field"
            )));
        }
    }
    Ok(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis)
}

/// The cues as one subtitle track, ready for the writer.
///
/// One sample per cue, payload a `vttc` box holding a `payl` box with the cue
/// text — the shape an ISO-BMFF player reads WebVTT from. The sample states its
/// own duration, so the gap between two cues does not stretch the first one.
pub fn track(cues: &[Cue]) -> Track {
    let mut samples = Vec::with_capacity(cues.len());
    // `stts` states how long each sample lasts, and the file's decode times are
    // the sum of those durations — they cannot be stated directly. So the decode
    // clock is the summed cue lengths, and the decode time that a player shows a
    // cue at comes from `ctts`: `pts` here is the cue's own time, `dts` the end
    // of the cue before it. A gap between two cues is therefore a positive
    // composition offset, which is exactly what it is.
    let mut decode_ms = 0u64;
    for cue in cues {
        // A zero-length cue is not a cue; `cues` refuses one, and the `max(1)`
        // keeps a hand-built `Cue` from producing a zero step in `stts`.
        let duration = u32::try_from(cue.end_ms.saturating_sub(cue.start_ms))
            .unwrap_or(u32::MAX)
            .max(1);
        samples.push(Sample {
            duration: Some(duration),
            data: vttc(&cue.text),
            pts: Some(cue.start_ms),
            dts: Some(decode_ms),
            keyframe: true,
        });
        decode_ms = decode_ms.saturating_add(u64::from(duration));
    }
    Track {
        kind: TrackKind::Subtitle,
        // Nothing here describes the *look* of the text: an empty `vttC` box
        // says so, and the player's own styling applies.
        codec: Codec::Wvtt,
        timescale: 1000,
        samples,
    }
}

/// `vttc` → `payl` → the cue text, which is what a WebVTT sample is.
fn vttc(text: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&((text.len() as u32) + 8).to_be_bytes());
    payload.extend_from_slice(b"payl");
    payload.extend_from_slice(text.as_bytes());
    let mut sample = Vec::new();
    sample.extend_from_slice(&((payload.len() as u32) + 8).to_be_bytes());
    sample.extend_from_slice(b"vttc");
    sample.extend_from_slice(&payload);
    sample
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
        // TTML is the other caption format a page can offer; saying so is more
        // use than "not WebVTT", because the answer for TTML is that this build
        // does not have a `stpp` writer, not that the body was unreadable.
        if looks_like_ttml(body) {
            return Err(Error::Refused(format!(
                "{url}: the caption track is TTML, which this build does not convert or \
                 write (there is no stpp writer); ask the page for WebVTT"
            )));
        }
        return Err(Error::Refused(format!(
            "{url}: the caption route answered {} bytes that are not WebVTT",
            bytes.len()
        )));
    }
    Ok(())
}

/// `<tt xmlns="http://www.w3.org/ns/ttml">` and the XML declaration that usually
/// precedes it, which is what a `ttml` caption route answers with.
fn looks_like_ttml(body: &[u8]) -> bool {
    let head = &body[..body.len().min(512)];
    head.windows(3).any(|window| window == b"<tt")
        && (head.starts_with(b"<?xml") || head.windows(4).any(|window| window == b"ttml"))
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
        assert!(
            !selection.matches("eng"),
            "a different language with the same prefix"
        );
        assert!(
            !selection.matches("en_US"),
            "the separator is the tag's own"
        );
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
    #[test]
    fn a_webvtt_body_becomes_cues() {
        // Deliberately out of order, with a comment block and a cue id, because
        // a real body has all three and the track's `stts` cannot.
        let body = b"WEBVTT\n\n\
                     NOTE this is not a cue\n\n\
                     00:02.000 --> 00:00:03.000\n\
                     last\n\n\
                     id-1\n\
                     00:00:01.000 --> 00:00:02.500 align:start line:0\n\
                     first line\nsecond line\n";
        let cues = cues(body).expect("a well-formed body parses");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1_000);
        assert_eq!(cues[0].end_ms, 2_500);
        assert_eq!(cues[0].text, "first line\nsecond line");
        assert_eq!(cues[1].start_ms, 2_000);
        assert_eq!(cues[1].text, "last");
    }

    #[test]
    fn timestamps_read_hours_minutes_and_milliseconds() {
        assert_eq!(timestamp("00:00:00.000").unwrap(), 0);
        assert_eq!(timestamp("00:00:01.500").unwrap(), 1_500);
        assert_eq!(timestamp("02:03.250").unwrap(), 123_250);
        assert_eq!(timestamp("01:02:03.004").unwrap(), 3_723_004);
        assert!(
            timestamp("00:00:01.5").is_err(),
            "a tenth is not a millisecond"
        );
        assert!(
            timestamp("00:61.000").is_err(),
            "sixty-one seconds in a clock"
        );
        assert!(timestamp("soon").is_err());
    }

    #[test]
    fn a_cue_that_ends_before_it_starts_is_refused() {
        let body = b"WEBVTT\n\n00:00:05.000 --> 00:00:04.000\nbackwards\n";
        let error = cues(body).unwrap_err();
        assert!(error.to_string().contains("no duration"), "{error}");
    }

    #[test]
    fn a_block_with_no_timing_line_is_refused() {
        let body = b"WEBVTT\n\nthis is not a cue\n\n";
        let error = cues(body).unwrap_err();
        assert!(error.to_string().contains("-->"), "{error}");
    }

    #[test]
    fn cues_become_one_subtitle_track() {
        let cues = vec![
            Cue {
                start_ms: 0,
                end_ms: 1_500,
                text: "hello".to_string(),
            },
            Cue {
                start_ms: 3_000,
                end_ms: 4_000,
                text: "world".to_string(),
            },
        ];
        let track = track(&cues);
        assert_eq!(track.kind, TrackKind::Subtitle);
        assert_eq!(track.timescale, 1_000);
        assert!(matches!(track.codec, Codec::Wvtt));
        assert_eq!(track.samples.len(), 2);
        assert_eq!(track.samples[0].dts, Some(0));
        assert_eq!(
            track.samples[0].duration,
            Some(1_500),
            "a cue states its own duration, gap or no gap"
        );
        assert_eq!(track.samples[1].duration, Some(1_000));
        assert_eq!(
            track.samples[1].dts,
            Some(1_500),
            "decode times are the summed durations, not the cue times"
        );
        assert_eq!(
            track.samples[1].pts,
            Some(3_000),
            "the cue keeps its own time, and the gap becomes a composition offset"
        );
        assert_eq!(
            &track.samples[0].data[4..8],
            b"vttc",
            "a sample is a vttc box"
        );
        assert_eq!(
            &track.samples[0].data[12..16],
            b"payl",
            "holding the text in payl"
        );
        assert_eq!(&track.samples[0].data[16..], b"hello");
    }

    #[test]
    fn a_ttml_body_is_refused_by_name() {
        let body = br#"<?xml version="1.0" encoding="utf-8"?>
<tt xmlns="http://www.w3.org/ns/ttml"><body/></tt>"#;
        let error = check_vtt(body, "https://example.test/captions").unwrap_err();
        assert!(error.to_string().contains("TTML"), "{error}");
        assert!(error.to_string().contains("stpp"), "{error}");
    }
}
