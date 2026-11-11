//! Vimeo.
//!
//! A public Vimeo video is read from the player's own configuration endpoint,
//! `https://player.vimeo.com/video/<id>/config` — the JSON the embed player
//! itself asks for. It lists progressive MP4s (`request.files.progressive`) and,
//! when the video has them, HLS renditions (`request.files.hls.cdns[*].url`).
//!
//! A progressive file is a complete MP4 — picture and sound in one stream — so
//! nothing has to be muxed; the playlist is kept for the videos that offer only
//! that, and `hlsfetch` assembles it like any other. DASH is in the same document
//! and is deliberately **not** read: it is `SegmentBase`, which this build does
//! not support, and a refusal that says so is more useful than a guess.
//!
//! An unlisted video needs the hash that its URL carries, so the hash is passed
//! back to the player as `?h=`. Without it the endpoint answers `404`, which is
//! the same answer a deleted video gets — the refusal says both.
//!
//! A **password-protected** video answers the player's config with `"view": 4`
//! — an empty shell — and the password is proved against
//! `<the player's page>/check-password`. That page is the config's own
//! `video.redirect_url`, which is the address the player redirects to when the
//! password is right, and asking the *user's* URL costs a request that answers
//! nothing. The correct answer is the config itself, so the second read replaces
//! the first rather than being added to it. The password comes from
//! [`crate::vimeo_password::from_environment`] and never from an argument.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::extractor_args::ExtractorArgs;
use crate::http::Http;
use crate::model::{Container, Format, Image, Media, Site, StreamKind};
use crate::sites::{at, list, text, uint};
use crate::url::Url;

const HEADERS: [(&str, &str); 2] = [
    ("referer", "https://vimeo.com/"),
    ("origin", "https://vimeo.com"),
];

pub fn extract(
    http: &Http,
    url: &Url,
    raw: &str,
    // This reader has no `--extractor-args` key; the parameter is uniform so
    // that the table of readers has one shape.
    _args: &ExtractorArgs,
) -> Result<Media> {
    let (id, hash) = video_id(url).ok_or_else(|| {
        Error::Usage(format!(
            "{raw} is not a video URL: expected https://vimeo.com/<id>, \
             https://vimeo.com/<id>/<hash> or https://player.vimeo.com/video/<id>"
        ))
    })?;

    let mut endpoint = format!("https://player.vimeo.com/video/{id}/config");
    if let Some(hash) = hash.as_deref() {
        endpoint.push_str("?h=");
        endpoint.push_str(hash);
    }
    let value = http.get_json_opt("vimeo-config", &endpoint, &HEADERS)?;
    let Some(value) = value else {
        return Err(Error::Refused(format!(
            "vimeo: {id}: the player answers 404, so the video is gone, private, or unlisted \
             without the hash its URL carries"
        )));
    };

    let page_url = match hash {
        Some(hash) => format!("https://vimeo.com/{id}/{hash}"),
        None => format!("https://vimeo.com/{id}"),
    };

    // `view: 4` is the player's own "this one is locked": the document is a
    // shell with no files in it, and parsing it would produce the "lists no
    // playable file" refusal even though the video is there.
    if at(&value, &["view"]).and_then(Value::as_u64) == Some(4) {
        return unlocked(http, &value, &id, page_url, raw);
    }
    parse(&value, &id, page_url)
}

/// The sentence a password-protected video gets when no password was given.
///
/// It names the variables and says *why* there is no flag, in one place, so the
/// answer a user gets and the answer a test checks cannot drift apart.
fn locked_refusal(id: &str, raw: &str) -> String {
    format!(
        "vimeo: {id}: the video is protected by a password, and none was given; set {} (or {} \
         to a file holding one) — a password is not a command-line argument, because {raw} \
         would then be in the process list and in the output of --help that gets pasted \
         into bug reports",
        crate::vimeo_password::PASSWORD_ENV,
        crate::vimeo_password::PASSWORD_FILE_ENV
    )
}

/// A locked player: the password is proved, and the config the endpoint answers
/// with is what `parse` reads.
fn unlocked(http: &Http, shell: &Value, id: &str, page_url: String, raw: &str) -> Result<Media> {
    let Some(password) = crate::vimeo_password::from_environment() else {
        return Err(Error::Refused(locked_refusal(id, raw)));
    };
    // The player's own address for this video, which is what the endpoint is
    // built on. Without it there is nothing to prove the password against.
    let Some(ticket) = text(shell, &["video", "redirect_url"]) else {
        return Err(Error::Refused(format!(
            "vimeo: {id}: the player says the video is locked and does not say where to prove a \
             password against, so this build cannot enter one"
        )));
    };
    match crate::vimeo_password::unlock_video(http, &ticket, &password)? {
        crate::vimeo_password::VideoAnswer::Wrong => Err(Error::Refused(format!(
            "vimeo: {id}: the password was not accepted (the player answers 418 for a wrong \
             one); nothing was retried and nothing was guessed"
        ))),
        crate::vimeo_password::VideoAnswer::Configuration(config) => parse(&config, id, page_url),
    }
}

/// The configuration document, read into a [`Media`].
///
/// Split from the fetch for the same reason the other extractors do it: the
/// shapes this reads are tested against a fixture, so a change on the site's
/// side produces a refusal in a bug report rather than a silent empty list.
pub fn parse(value: &Value, id: &str, page_url: String) -> Result<Media> {
    let mut media = Media::new(Site::Vimeo, id.to_string(), page_url);
    media.title = text(value, &["video", "title"]).unwrap_or_default();
    if media.title.is_empty() {
        media.title = format!("vimeo {id}");
    }
    media.uploader = text(value, &["video", "owner", "name"]);
    media.duration = at(value, &["video", "duration"])
        .and_then(Value::as_f64)
        .map(|seconds| seconds.round().max(0.0) as u64)
        .filter(|seconds| *seconds > 0);
    media.thumbnail = thumbnail(value);

    for (index, file) in list(value, &["request", "files", "progressive"])
        .iter()
        .enumerate()
    {
        let Some(file_url) = text(file, &["url"]) else {
            continue;
        };
        // Anything that is not an MP4 would have to be re-encoded or handed on
        // as a container this build cannot write, so it is not offered.
        let mime = text(file, &["mime"]).unwrap_or_default();
        if !mime.is_empty() && !mime.contains("mp4") {
            continue;
        }
        let quality = text(file, &["quality"]).unwrap_or_else(|| format!("{}", index + 1));
        media.formats.push(Format {
            id: format!("vimeo-{quality}"),
            kind: StreamKind::Muxed,
            container: Container::Mp4,
            codec: String::new(),
            width: uint(file, &["width"]).unwrap_or(0) as u32,
            height: uint(file, &["height"]).unwrap_or(0) as u32,
            fps: uint(file, &["fps"]).unwrap_or(0) as u32,
            bitrate: uint(file, &["bitrate"]).unwrap_or(0),
            bytes: None,
            url: Some(file_url),
            headers: request_headers(),
            source: "vimeo-config",
        });
    }

    if let Some(hls) = hls_url(value) {
        // Kept beside the progressive files rather than instead of them: it
        // states no size, so selection prefers a progressive MP4 on its own, and
        // `--format vimeo-hls` can still ask for this one.
        media.formats.push(Format {
            id: "vimeo-hls".to_string(),
            kind: StreamKind::Muxed,
            container: Container::Hls,
            codec: String::new(),
            width: 0,
            height: 0,
            fps: 0,
            bitrate: 0,
            bytes: None,
            url: Some(hls),
            headers: request_headers(),
            source: "hls",
        });
    }

    if media.formats.is_empty() {
        if at(value, &["request", "files", "dash"]).is_some() {
            return Err(Error::Refused(format!(
                "vimeo: {id}: the video is offered only as DASH (SegmentBase), which this build \
                 does not read; there is no progressive MP4 and no HLS playlist to take"
            )));
        }
        return Err(Error::Refused(format!(
            "vimeo: {id}: the player lists no playable file — the video is private or blocked \
             from embedding; --cookies FILE is sent when you name a file, and no challenge is \
             solved"
        )));
    }

    Ok(media)
}

/// The widest poster the document lists. Vimeo keys `thumbs` by the width of the
/// image, which is why the key is parsed rather than the order trusted.
fn thumbnail(value: &Value) -> Option<Image> {
    let thumbs = at(value, &["video", "thumbs"])?.as_object()?;
    let (_, url) = thumbs
        .iter()
        .filter_map(|(width, url)| Some((width.parse::<u32>().ok()?, url.as_str()?)))
        .max_by_key(|(width, _)| *width)?;
    Some(Image {
        url: url.to_string(),
        extension: "jpg",
        headers: request_headers(),
    })
}

/// The playlist to hand to `hlsfetch`.
///
/// Several CDNs are listed for the same playlist. They are the same bytes, so
/// the choice is about who answers without a token: the Google origin first,
/// then whatever else is there.
fn hls_url(value: &Value) -> Option<String> {
    if let Some(cdns) = at(value, &["request", "files", "hls", "cdns"]).and_then(Value::as_object) {
        let mut best: Option<(u8, String)> = None;
        for (name, entry) in cdns {
            let Some(url) = entry.get("url").and_then(Value::as_str) else {
                continue;
            };
            let rank = if name.as_str() == "gcs" { 0 } else { 1 };
            if best
                .as_ref()
                .map(|(ranked, _)| rank < *ranked)
                .unwrap_or(true)
            {
                best = Some((rank, url.to_string()));
            }
        }
        if let Some((_, url)) = best {
            return Some(url);
        }
    }
    text(value, &["request", "files", "hls", "url"])
}

fn request_headers() -> Vec<(String, String)> {
    vec![
        (
            "user-agent".to_string(),
            crate::http::UA_DESKTOP.to_string(),
        ),
        ("referer".to_string(), "https://vimeo.com/".to_string()),
    ]
}

/// `<id>` and the unlisted hash, from every URL shape Vimeo uses.
fn video_id(url: &Url) -> Option<(String, Option<String>)> {
    let segments = url.segments();
    if url.host() == Some("player.vimeo.com") {
        let at_video = segments.iter().position(|segment| *segment == "video")?;
        let id = segments.get(at_video + 1)?;
        if !is_id(id) {
            return None;
        }
        let hash = url.query_param("h").filter(|hash| is_hash(hash));
        return Some((id.to_string(), hash));
    }

    // The last all-digit segment is the id; a segment after it is the unlisted
    // hash: `/76979871/1a2b3c4d5e`, `/channels/staffpicks/76979871`.
    let index = segments.iter().rposition(|segment| is_id(segment))?;
    let id = segments[index].to_string();
    let hash = segments
        .get(index + 1)
        .filter(|segment| is_hash(segment))
        .map(|segment| (*segment).to_string());
    Some((id, hash))
}

fn is_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 12 && value.chars().all(|c| c.is_ascii_digit())
}

fn is_hash(value: &str) -> bool {
    (8..=16).contains(&value.len()) && value.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Value {
        json!({
            "video": {
                "id": 76979871,
                "title": "The New Vimeo Player",
                "duration": 62.4,
                "owner": { "name": "Vimeo" },
                "thumbs": {
                    "295": "https://i.vimeocdn.com/video/295x166.jpg",
                    "1280": "https://i.vimeocdn.com/video/1280x720.jpg"
                }
            },
            "request": {
                "files": {
                    "progressive": [
                        {
                            "quality": "360p",
                            "width": 640,
                            "height": 360,
                            "fps": 25,
                            "bitrate": 800000,
                            "mime": "video/mp4",
                            "url": "https://vod-progressive.example/a.mp4"
                        },
                        {
                            "quality": "1080p",
                            "width": 1920,
                            "height": 1080,
                            "fps": 25,
                            "bitrate": 4000000,
                            "mime": "video/mp4",
                            "url": "https://vod-progressive.example/b.mp4"
                        }
                    ],
                    "hls": {
                        "cdns": {
                            "fastly_skyfire": { "url": "https://skyfire.example/x.m3u8" },
                            "gcs": { "url": "https://gcs.example/y.m3u8" }
                        }
                    },
                    "dash": { "cdns": {} }
                }
            }
        })
    }

    #[test]
    fn a_public_video_is_read_from_the_player_config() {
        let media = parse(&fixture(), "76979871", "https://vimeo.com/76979871".into())
            .expect("a public video parses");
        assert_eq!(media.site, Site::Vimeo);
        assert_eq!(media.title, "The New Vimeo Player");
        assert_eq!(media.uploader.as_deref(), Some("Vimeo"));
        assert_eq!(
            media.duration,
            Some(62),
            "the float is rounded, not truncated"
        );
        assert_eq!(
            media.thumbnail.as_ref().map(|image| image.url.as_str()),
            Some("https://i.vimeocdn.com/video/1280x720.jpg"),
            "the widest poster wins"
        );
        let mp4: Vec<&Format> = media
            .formats
            .iter()
            .filter(|format| format.container == Container::Mp4)
            .collect();
        assert_eq!(mp4.len(), 2);
        assert_eq!(mp4[1].id, "vimeo-1080p");
        assert_eq!(mp4[1].height, 1080);
        assert!(mp4[1].url.is_some());
        let hls: Vec<&Format> = media
            .formats
            .iter()
            .filter(|format| format.container == Container::Hls)
            .collect();
        assert_eq!(hls.len(), 1);
        assert_eq!(
            hls[0].url.as_deref(),
            Some("https://gcs.example/y.m3u8"),
            "the Google origin is preferred"
        );
    }

    #[test]
    fn a_locked_player_is_recognised_by_its_own_view_field() {
        // `view: 4` is the player's shell document: no files, and the password
        // is proved against `video.redirect_url`. Parsing it as an ordinary
        // config produces "lists no playable file", which is why it is checked
        // before `parse` runs at all.
        let shell = json!({
            "view": 4,
            "video": { "title": "locked", "redirect_url": "https://player.vimeo.com/video/1?h=abc" },
            "request": { "files": {} }
        });
        assert_eq!(
            at(&shell, &["view"]).and_then(Value::as_u64),
            Some(4),
            "a public config has no `view` at all, so this cannot false-positive"
        );
        assert_eq!(
            text(&shell, &["video", "redirect_url"]).as_deref(),
            Some("https://player.vimeo.com/video/1?h=abc"),
            "and the page it names is where the password is proved"
        );
        let public = fixture();
        assert_eq!(at(&public, &["view"]).and_then(Value::as_u64), None);
    }

    #[test]
    fn the_password_refusal_names_the_variable_and_not_a_flag() {
        // The refusal has to point at the thing that works, and it must not
        // suggest an argument that does not exist. The environment is read once,
        // by `unlocked`, and a test that set it would change how every other
        // test in this build runs — so the sentence is what is checked.
        let text = locked_refusal("1", "https://vimeo.com/1");
        assert!(text.contains("TAFAS_VIDEO_PASSWORD"), "{text}");
        assert!(
            text.contains("process list"),
            "it says why not a flag: {text}"
        );
    }

    #[test]
    fn a_video_with_no_playable_file_is_refused() {
        let value = json!({ "video": { "title": "x" }, "request": { "files": {} } });
        let error = parse(&value, "1", "https://vimeo.com/1".into()).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("password-protected"), "{text}");
    }

    #[test]
    fn dash_only_is_refused_by_name() {
        let value = json!({
            "request": { "files": { "dash": { "cdns": { "gcs": { "url": "https://x/y.mpd" } } } } }
        });
        let error = parse(&value, "1", "https://vimeo.com/1".into()).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("DASH"), "{text}");
        assert!(text.contains("SegmentBase"), "{text}");
    }

    #[test]
    fn ids_come_from_every_url_shape() {
        let id = |url: &str| video_id(&Url::parse(url).unwrap());
        assert_eq!(
            id("https://vimeo.com/76979871"),
            Some(("76979871".into(), None))
        );
        assert_eq!(
            id("https://www.vimeo.com/76979871"),
            Some(("76979871".into(), None))
        );
        assert_eq!(
            id("https://vimeo.com/channels/staffpicks/76979871"),
            Some(("76979871".into(), None))
        );
        assert_eq!(
            id("https://vimeo.com/76979871/1a2b3c4d5e"),
            Some(("76979871".into(), Some("1a2b3c4d5e".into())))
        );
        assert_eq!(
            id("https://player.vimeo.com/video/76979871?h=1a2b3c4d5e"),
            Some(("76979871".into(), Some("1a2b3c4d5e".into())))
        );
        assert_eq!(id("https://vimeo.com/channels/staffpicks"), None);
        assert_eq!(id("https://vimeo.com/"), None);
    }
}
