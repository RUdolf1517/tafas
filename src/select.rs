//! Which stream to fetch, and in which shape to write it.
//!
//! The rules are short and stated in `docs/cli.md`:
//!
//! * `best` — the highest resolution, then the highest bitrate, then the codec
//!   most players accept without a plugin;
//! * `720p` (any height) — the best stream **not above** that height; when a
//!   video has nothing that small, the smallest available is used and the
//!   substitution is reported rather than hidden;
//! * `worst` — the lowest resolution and bitrate;
//! * audio-only — the best audio stream, preferring the container that can be
//!   written without transcoding (`m4a`).
//!
//! A video stream and an audio stream are combined into one file only when both
//! are ISO-BMFF, because that is what `mux::remux` can join without re-encoding.
//! Everything else is downloaded as separate files, and the terminal says so.

use crate::error::{Error, Result};
use crate::model::{Container, Format, Image, Media, StreamKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Best,
    Worst,
    Height(u32),
}

impl Quality {
    pub fn parse(text: &str) -> Result<Quality> {
        let lower = text.trim().to_ascii_lowercase();
        match lower.as_str() {
            "best" | "max" | "highest" => return Ok(Quality::Best),
            "worst" | "min" | "lowest" => return Ok(Quality::Worst),
            _ => {}
        }
        let digits = lower.strip_suffix('p').unwrap_or(&lower);
        match digits.parse::<u32>() {
            Ok(height) if height > 0 => Ok(Quality::Height(height)),
            _ => Err(Error::Usage(format!(
                "unknown quality '{text}': expected best, worst, or a height such as 720p"
            ))),
        }
    }

    pub fn as_str(self) -> String {
        match self {
            Quality::Best => "best".into(),
            Quality::Worst => "worst".into(),
            Quality::Height(h) => format!("{h}p"),
        }
    }
}

/// What the downloader has to fetch.
#[derive(Debug, Clone)]
pub enum Plan {
    /// One stream that already carries picture and sound.
    Single(Format),
    /// Two ISO-BMFF streams that become one file.
    Pair { video: Format, audio: Format },
    /// Two streams written next to each other, because they cannot be joined.
    Separate {
        video: Format,
        audio: Option<Format>,
        why: Option<String>,
    },
    /// Sound only.
    Audio(Format),
    /// Still images.
    Images(Vec<Image>),
}

impl Plan {
    /// The file extension the plan produces, for naming and for `--dry-run`.
    pub fn extension(&self) -> &'static str {
        match self {
            Plan::Single(format) | Plan::Separate { video: format, .. } => {
                format.container.extension()
            }
            Plan::Pair { video, .. } => video.container.extension(),
            Plan::Audio(format) => match format.container {
                Container::Mp4 => "m4a",
                Container::Webm => "webm",
                other => other.extension(),
            },
            Plan::Images(_) => "jpg",
        }
    }

    pub fn is_muxed(&self) -> bool {
        matches!(self, Plan::Pair { .. })
    }

    /// An approximate size, used only by `--dry-run` and the progress total.
    pub fn estimate_bytes(&self, seconds: u64) -> Option<u64> {
        match self {
            Plan::Single(format) => format.estimate_bytes(seconds),
            Plan::Pair { video, audio } => {
                let video_bytes = video.estimate_bytes(seconds)?;
                let audio_bytes = audio.estimate_bytes(seconds)?;
                Some(video_bytes + audio_bytes)
            }
            Plan::Separate { video, audio, .. } => {
                let mut total = video.estimate_bytes(seconds)?;
                if let Some(audio) = audio {
                    total += audio.estimate_bytes(seconds)?;
                }
                Some(total)
            }
            Plan::Audio(format) => format.estimate_bytes(seconds),
            Plan::Images(_) => None,
        }
    }

    /// Short description for the human report.
    pub fn describe(&self) -> String {
        match self {
            Plan::Single(format) => format!("{} {}", format.label(), format.container_name()),
            Plan::Pair { video, audio } => format!(
                "{} {} + {} audio (remuxed)",
                video.label(),
                video.container_name(),
                audio.codec_or_container()
            ),
            Plan::Separate { video, audio, .. } => match audio {
                Some(audio) => format!(
                    "{} {} + {} audio (separate files)",
                    video.label(),
                    video.container_name(),
                    audio.codec_or_container()
                ),
                None => format!(
                    "{} {} (no audio track)",
                    video.label(),
                    video.container_name()
                ),
            },
            Plan::Audio(format) => format!("{} audio", format.codec_or_container()),
            Plan::Images(images) => format!("{} image(s)", images.len()),
        }
    }
}

impl Format {
    fn container_name(&self) -> &'static str {
        self.container.name()
    }

    fn codec_or_container(&self) -> String {
        if self.codec.is_empty() {
            self.container_name().to_string()
        } else {
            self.codec.clone()
        }
    }
}

/// Everything the caller may constrain.
#[derive(Debug, Clone)]
pub struct Options {
    pub quality: Quality,
    pub audio_only: bool,
    /// An exact stream id (`itag`), from `--format`.
    pub format: Option<String>,
    /// `--no-mux`: keep the streams apart even when they could be joined.
    pub no_mux: bool,
}

pub fn choose(media: &Media, options: &Options) -> Result<Plan> {
    let playable: Vec<&Format> = media
        .formats
        .iter()
        .filter(|format| format.url.is_some())
        .collect();

    if let Some(wanted) = &options.format {
        let format = playable
            .iter()
            .find(|format| &format.id == wanted)
            .copied()
            .ok_or_else(|| {
                let available: Vec<&str> =
                    playable.iter().map(|format| format.id.as_str()).collect();
                Error::Usage(format!(
                    "this video has no format '{wanted}'; available: {}",
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                ))
            })?;
        return match format.kind {
            StreamKind::Muxed => Ok(Plan::Single(Format::clone(format))),
            StreamKind::Audio => Ok(Plan::Audio(Format::clone(format))),
            StreamKind::Video => {
                let audio = best_audio(&playable);
                Ok(join(Format::clone(format), audio, options.no_mux))
            }
        };
    }

    if options.audio_only {
        let audio = best_audio(&playable).ok_or_else(|| {
            Error::Refused(format!(
                "{}: this page has no audio-only stream; audio is muxed into the video",
                media.site
            ))
        })?;
        return Ok(Plan::Audio(audio));
    }

    let videos: Vec<&Format> = playable
        .iter()
        .copied()
        .filter(|format| format.kind.has_video())
        .collect();

    if videos.is_empty() {
        if !media.images.is_empty() {
            return Ok(Plan::Images(media.images.clone()));
        }
        if let Some(audio) = best_audio(&playable) {
            return Ok(Plan::Audio(audio));
        }
        return Err(Error::Unsupported(format!(
            "{}: this page has nothing tafas can download",
            media.site
        )));
    }

    // A video-only stream is preferred: it is the only way to reach the
    // resolutions above 720p, and pairing it is what most users mean by "best".
    let video_only: Vec<&Format> = videos
        .iter()
        .copied()
        .filter(|format| format.kind == StreamKind::Video)
        .collect();

    let chosen_video =
        pick_video(&video_only, options.quality).or_else(|| pick_video(&videos, options.quality));

    let Some(video) = chosen_video else {
        return Err(Error::Unsupported(format!(
            "{}: no video stream matched {}",
            media.site,
            options.quality.as_str()
        )));
    };

    if video.kind == StreamKind::Video {
        let audio = best_audio(&playable);
        return Ok(join(video.clone(), audio, options.no_mux));
    }

    // Already muxed: nothing to join, and asking for a height that only exists
    // as a separate stream would otherwise have produced an error.
    Ok(Plan::Single(video.clone()))
}

/// Decides between one file and two.
fn join(video: Format, audio: Option<Format>, no_mux: bool) -> Plan {
    match audio {
        None => Plan::Separate {
            video,
            audio: None,
            why: None,
        },
        Some(audio) => {
            let joinable = video.container.is_isobmff() && audio.container.is_isobmff();
            if joinable && !no_mux {
                Plan::Pair { video, audio }
            } else {
                let why = if no_mux {
                    Some("--no-mux was given".to_string())
                } else if !joinable {
                    Some(format!(
                        "the audio stream is {}, which cannot be remuxed with {} video without \
                         re-encoding",
                        audio.codec_or_container(),
                        video.container_name()
                    ))
                } else {
                    None
                };
                Plan::Separate {
                    video,
                    audio: Some(audio),
                    why,
                }
            }
        }
    }
}

fn codec_rank(codec: &str) -> u8 {
    // Compatibility first: a file that every player opens beats a smaller one
    // that half of them refuse.
    if codec.starts_with("avc1") || codec.starts_with("mp4a") {
        3
    } else if codec.starts_with("av01") {
        2
    } else if codec.starts_with("vp9") || codec.starts_with("vp09") || codec.starts_with("opus") {
        1
    } else {
        0
    }
}

fn pick_video(candidates: &[&Format], quality: Quality) -> Option<Format> {
    if candidates.is_empty() {
        return None;
    }
    let scored = |format: &Format| -> (u32, u64, u8) {
        (format.height, format.bitrate, codec_rank(&format.codec))
    };

    match quality {
        Quality::Best => candidates
            .iter()
            .max_by_key(|format| scored(format))
            .map(|format| Format::clone(*format)),
        Quality::Worst => candidates
            .iter()
            .min_by_key(|format| scored(format))
            .map(|format| Format::clone(*format)),
        Quality::Height(height) => {
            let fitting: Vec<&&Format> = candidates
                .iter()
                .filter(|format| format.height > 0 && format.height <= height)
                .collect();
            if !fitting.is_empty() {
                return fitting
                    .iter()
                    .max_by_key(|format| scored(format))
                    .map(|format| (***format).clone());
            }
            // Nothing that small exists: take the closest above it rather than
            // failing a request the user can hardly act on.
            candidates
                .iter()
                .filter(|format| format.height > 0)
                .min_by_key(|format| (format.height, format.bitrate))
                .map(|format| Format::clone(*format))
        }
    }
}

fn best_audio(candidates: &[&Format]) -> Option<Format> {
    candidates
        .iter()
        .copied()
        .filter(|format| format.kind == StreamKind::Audio)
        .max_by_key(|format| {
            (
                // An m4a can be joined with an mp4 video; a webm cannot.
                u8::from(format.container.is_isobmff()),
                format.bitrate,
                codec_rank(&format.codec),
            )
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Site;

    fn format(
        id: &str,
        kind: StreamKind,
        container: Container,
        codec: &str,
        height: u32,
        bitrate: u64,
    ) -> Format {
        Format {
            id: id.into(),
            kind,
            container,
            codec: codec.into(),
            width: 0,
            height,
            fps: 30,
            bitrate,
            bytes: None,
            url: Some(format!("https://example/{id}")),
            headers: Vec::new(),
            source: "test",
        }
    }

    fn media(formats: Vec<Format>) -> Media {
        let mut media = Media::new(Site::YouTube, "id".into(), "https://example".into());
        media.title = "a video".into();
        media.formats = formats;
        media
    }

    fn options(quality: Quality) -> Options {
        Options {
            quality,
            audio_only: false,
            format: None,
            no_mux: false,
        }
    }

    fn dash(media: &Media) -> Vec<Format> {
        let _ = media;
        vec![
            format(
                "137",
                StreamKind::Video,
                Container::Mp4,
                "avc1.640028",
                1080,
                4_000_000,
            ),
            format(
                "136",
                StreamKind::Video,
                Container::Mp4,
                "avc1.4d401f",
                720,
                2_000_000,
            ),
            format(
                "140",
                StreamKind::Audio,
                Container::Mp4,
                "mp4a.40.2",
                0,
                128_000,
            ),
            format(
                "251",
                StreamKind::Audio,
                Container::Webm,
                "opus",
                0,
                160_000,
            ),
        ]
    }

    #[test]
    fn best_pairs_the_highest_video_with_mp4_audio() {
        let media = media(dash(&media_placeholder()));
        let plan = choose(&media, &options(Quality::Best)).unwrap();
        match plan {
            Plan::Pair { video, audio } => {
                assert_eq!(video.id, "137");
                // Opus is higher bitrate, but it cannot be joined with an mp4
                // video, so the m4a wins.
                assert_eq!(audio.id, "140");
            }
            other => panic!("expected a pair, got {other:?}"),
        }
    }

    fn media_placeholder() -> Media {
        Media::new(Site::YouTube, "id".into(), "https://example".into())
    }

    #[test]
    fn a_height_caps_the_choice() {
        let media = media(dash(&media_placeholder()));
        let plan = choose(&media, &options(Quality::Height(720))).unwrap();
        match plan {
            Plan::Pair { video, .. } => assert_eq!(video.id, "136"),
            other => panic!("expected a pair, got {other:?}"),
        }
    }

    #[test]
    fn a_height_below_everything_takes_the_smallest() {
        let media = media(dash(&media_placeholder()));
        let plan = choose(&media, &options(Quality::Height(240))).unwrap();
        match plan {
            Plan::Pair { video, .. } => assert_eq!(video.id, "136"),
            other => panic!("expected a pair, got {other:?}"),
        }
    }

    #[test]
    fn worst_takes_the_lowest_stream() {
        let media = media(dash(&media_placeholder()));
        let plan = choose(&media, &options(Quality::Worst)).unwrap();
        match plan {
            Plan::Pair { video, .. } => assert_eq!(video.id, "136"),
            other => panic!("expected a pair, got {other:?}"),
        }
    }

    #[test]
    fn no_mux_keeps_the_streams_apart() {
        let media = media(dash(&media_placeholder()));
        let mut opts = options(Quality::Best);
        opts.no_mux = true;
        match choose(&media, &opts).unwrap() {
            Plan::Separate { video, audio, why } => {
                assert_eq!(video.id, "137");
                assert_eq!(audio.map(|a| a.id), Some("140".into()));
                assert!(why.unwrap().contains("--no-mux"));
            }
            other => panic!("expected separate files, got {other:?}"),
        }
    }

    #[test]
    fn a_progressive_stream_is_used_when_no_dash_pair_exists() {
        let media = media(vec![format(
            "18",
            StreamKind::Muxed,
            Container::Mp4,
            "avc1.42001E",
            360,
            500_000,
        )]);
        match choose(&media, &options(Quality::Best)).unwrap() {
            Plan::Single(format) => assert_eq!(format.id, "18"),
            other => panic!("expected a single stream, got {other:?}"),
        }
    }

    #[test]
    fn audio_only_prefers_m4a() {
        let media = media(dash(&media_placeholder()));
        let mut opts = options(Quality::Best);
        opts.audio_only = true;
        match choose(&media, &opts).unwrap() {
            Plan::Audio(format) => assert_eq!(format.id, "140"),
            other => panic!("expected audio, got {other:?}"),
        }
    }

    #[test]
    fn an_exact_format_is_honoured_and_a_missing_one_is_a_usage_error() {
        let media = media(dash(&media_placeholder()));
        let mut opts = options(Quality::Best);
        opts.format = Some("136".into());
        match choose(&media, &opts).unwrap() {
            Plan::Pair { video, .. } => assert_eq!(video.id, "136"),
            other => panic!("expected a pair, got {other:?}"),
        }

        let mut missing = options(Quality::Best);
        missing.format = Some("999".into());
        let error = choose(&media, &missing).unwrap_err();
        assert!(error.is_usage());
        assert!(error.to_string().contains("available"));
    }

    #[test]
    fn images_are_used_when_there_is_no_video() {
        let mut media = media(vec![]);
        media.images.push(Image {
            url: "https://example/1.jpg".into(),
            extension: "jpg",
            headers: Vec::new(),
        });
        match choose(&media, &options(Quality::Best)).unwrap() {
            Plan::Images(images) => assert_eq!(images.len(), 1),
            other => panic!("expected images, got {other:?}"),
        }
    }

    #[test]
    fn qualities_parse_and_reject() {
        assert_eq!(Quality::parse("best").unwrap(), Quality::Best);
        assert_eq!(Quality::parse("1080p").unwrap(), Quality::Height(1080));
        assert_eq!(Quality::parse("480").unwrap(), Quality::Height(480));
        assert_eq!(Quality::parse("worst").unwrap(), Quality::Worst);
        assert!(Quality::parse("huge").unwrap_err().is_usage());
        assert!(Quality::parse("0p").unwrap_err().is_usage());
    }
}
