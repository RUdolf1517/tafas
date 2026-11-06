//! YouTube.
//!
//! Two routes, tried in this order:
//!
//! 1. the watch page, whose inlined `ytInitialPlayerResponse` is the same
//!    document the browser receives;
//! 2. InnerTube — the JSON API the site's own clients call — with a fixed
//!    order of clients (`ANDROID_VR`, `IOS`, `ANDROID`, embedded TV, `WEB`).
//!
//! The order is not taste. YouTube decides what to serve partly from the client
//! that asks, and some clients receive directly playable URLs while others
//! receive a cipher. A format that arrives as `signatureCipher` is *dropped*,
//! never guessed at: solving the signature means executing the site's own
//! minified player JavaScript, which this build does not do. If every format of
//! a video is protected that way, the run ends in a documented refusal rather
//! than in a broken file.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::http::{Http, UA_ANDROID, UA_DESKTOP, UA_IOS};
use crate::model::{Container, Format, Media, Site, StreamKind};
use crate::sites::{at, json_after, list, text, uint};
use crate::url::Url;

/// The InnerTube key that youtube.com serves to an anonymous browser. It is a
/// public identifier embedded in the site's own HTML, not a credential; the
/// page's value is preferred whenever it can be read, and this one only fills
/// the gap when the page could not be fetched at all.
const FALLBACK_API_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";

const UA_ANDROID_VR: &str =
    "com.google.android.apps.youtube.vr.oculus/1.60.19 (Linux; U; Android 12; GB) gzip";

/// Consent and locale cookies. Without them a European request is answered with
/// the consent interstitial, which contains no player response at all.
const PAGE_HEADERS: [(&str, &str); 3] = [
    ("accept-language", "en-US,en;q=0.9"),
    ("cookie", "CONSENT=YES+cb; SOCS=CAI"),
    ("accept", "text/html,application/xhtml+xml"),
];

pub fn extract(http: &Http, url: &Url, raw: &str) -> Result<Media> {
    let id = video_id(url).ok_or_else(|| {
        Error::Usage(format!(
            "{raw} is not a YouTube video URL: expected /watch?v=ID, /shorts/ID or youtu.be/ID"
        ))
    })?;

    let mut media = Media::new(
        Site::YouTube,
        id.clone(),
        format!("https://www.youtube.com/watch?v={id}"),
    );
    let mut notes: Vec<String> = Vec::new();

    let watch_url =
        format!("https://www.youtube.com/watch?v={id}&hl=en&bpctr=9999999999&has_verified=1");
    let page = match http.get_text(&watch_url, &PAGE_HEADERS) {
        Ok(page) => Some(page),
        Err(error) => {
            // A missing watch page is not fatal: the API route below does not
            // need it. It costs the API key, so the fallback key is used.
            notes.push(format!("watch page: {error}"));
            None
        }
    };

    let mut api_key = FALLBACK_API_KEY.to_string();
    if let Some(page) = &page {
        if let Some(key) = inline_string(page, "INNERTUBE_API_KEY") {
            api_key = key;
        }
        if let Some(document) = json_after(page, "ytInitialPlayerResponse = ") {
            if let Ok(value) = serde_json::from_str::<Value>(document) {
                let info = read_player(&value, "watch-page");
                if !info.formats.is_empty() {
                    apply(&mut media, info);
                    return Ok(media);
                }
                notes.push(format!("watch page: {}", describe(&info)));
            } else {
                notes.push("watch page: the inlined player response did not parse".into());
            }
        }
    }

    let endpoint =
        format!("https://www.youtube.com/youtubei/v1/player?key={api_key}&prettyPrint=false");
    for (name, user_agent, context) in clients() {
        let body = json!({
            "context": context,
            "videoId": id,
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        let headers = [
            ("user-agent", user_agent),
            ("origin", "https://www.youtube.com"),
        ];
        match http.post_json(name, &endpoint, &headers, &body) {
            Ok(value) => {
                let info = read_player(&value, name);
                if !info.formats.is_empty() {
                    apply(&mut media, info);
                    return Ok(media);
                }
                notes.push(format!("{name}: {}", describe(&info)));
            }
            Err(error) => notes.push(format!("{name}: {error}")),
        }
    }

    Err(refusal(&id, &notes))
}

/// Turns the accumulated failure reasons into the one sentence the user gets.
/// A bot check and a cipher are different problems with different answers, so
/// they are named separately instead of being flattened into "failed".
fn refusal(id: &str, notes: &[String]) -> Error {
    let joined = notes.join("; ").to_ascii_lowercase();
    if joined.contains("sign in to confirm") || joined.contains("not a bot") {
        return Error::Refused(format!(
            "youtube: {id}: the site answered with a sign-in check (\"{}\"). Cookies from \
             --cookies are sent when you name a file, but no challenge is solved; see \
             docs/extractors.md §3.",
            first_bot_sentence(notes)
        ));
    }
    if joined.contains("signature") || joined.contains("cipher") {
        return Error::Refused(format!(
            "youtube: {id}: every format is signature-protected; this build does not execute the \
             player JavaScript that would decipher it (docs/extractors.md §4)"
        ));
    }
    if joined.contains("age") || joined.contains("inappropriate") {
        return Error::Refused(format!(
            "youtube: {id}: age-restricted content needs a signed-in account; pass --cookies FILE \
             with an export from a signed-in browser (docs/extractors.md §3)"
        ));
    }
    Error::Refused(format!(
        "youtube: {id} could not be read: {}",
        if notes.is_empty() {
            "no client returned a playable format".to_string()
        } else {
            notes.join("; ")
        }
    ))
}

fn first_bot_sentence(notes: &[String]) -> String {
    for note in notes {
        let lower = note.to_ascii_lowercase();
        if let Some(at) = lower.find("sign in to confirm") {
            let tail: String = note[at..].chars().take(90).collect();
            return tail;
        }
    }
    "Sign in to confirm you're not a bot".into()
}

/// The client chain, in the order it is tried.
fn clients() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "android_vr",
            UA_ANDROID_VR,
            json!({"client": {
                "clientName": "ANDROID_VR",
                "clientVersion": "1.60.19",
                "androidSdkVersion": 32,
                "deviceMake": "Oculus",
                "deviceModel": "Quest 3",
                "osName": "Android",
                "osVersion": "12",
                "hl": "en",
                "gl": "US"
            }}),
        ),
        (
            "ios",
            UA_IOS,
            json!({"client": {
                "clientName": "IOS",
                "clientVersion": "19.45.4",
                "deviceMake": "Apple",
                "deviceModel": "iPhone16,2",
                "osName": "iPhone",
                "osVersion": "18.1.0.22B83",
                "hl": "en",
                "gl": "US"
            }}),
        ),
        (
            "android",
            UA_ANDROID,
            json!({"client": {
                "clientName": "ANDROID",
                "clientVersion": "19.44.38",
                "androidSdkVersion": 34,
                "osName": "Android",
                "osVersion": "14",
                "hl": "en",
                "gl": "US"
            }}),
        ),
        (
            "tv_embedded",
            UA_DESKTOP,
            json!({
                "client": {
                    "clientName": "TVHTML5_SIMPLY_EMBEDDED_PLAYER",
                    "clientVersion": "2.0",
                    "hl": "en",
                    "gl": "US"
                },
                "thirdParty": {"embedUrl": "https://www.youtube.com/"}
            }),
        ),
        (
            "web",
            UA_DESKTOP,
            json!({"client": {
                "clientName": "WEB",
                "clientVersion": "2.20241126.01.00",
                "hl": "en",
                "gl": "US"
            }}),
        ),
    ]
}

/// What one player document said, whether or not it was usable.
struct PlayerInfo {
    title: Option<String>,
    uploader: Option<String>,
    duration: Option<u64>,
    formats: Vec<Format>,
    /// `None` when the document reported `OK`; otherwise the status and reason.
    status: Option<String>,
    /// Formats that arrived as a cipher and were therefore dropped.
    ciphered: usize,
}

fn read_player(value: &Value, source: &'static str) -> PlayerInfo {
    let status_code = at(value, &["playabilityStatus", "status"])
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN");
    let reason = text(value, &["playabilityStatus", "reason"]).or_else(|| {
        text(
            value,
            &[
                "playabilityStatus",
                "errorScreen",
                "playerErrorMessageRenderer",
                "subreason",
                "simpleText",
            ],
        )
    });

    let mut formats = Vec::new();
    let mut ciphered = 0usize;
    for section in ["formats", "adaptiveFormats"] {
        for item in list(value, &["streamingData", section]) {
            if item.get("url").and_then(|v| v.as_str()).is_some() {
                if let Some(format) = format_from(item, source) {
                    formats.push(format);
                }
            } else if item.get("signatureCipher").is_some() || item.get("cipher").is_some() {
                ciphered += 1;
            }
        }
    }

    PlayerInfo {
        title: text(value, &["videoDetails", "title"]),
        uploader: text(value, &["videoDetails", "author"]),
        duration: uint(value, &["videoDetails", "lengthSeconds"]),
        formats,
        status: if status_code == "OK" {
            None
        } else {
            Some(match reason {
                Some(reason) => format!("{status_code}: {reason}"),
                None => status_code.to_string(),
            })
        },
        ciphered,
    }
}

fn describe(info: &PlayerInfo) -> String {
    match &info.status {
        Some(status) => status.clone(),
        None if info.ciphered > 0 && info.formats.is_empty() => {
            format!("only signature-protected formats ({})", info.ciphered)
        }
        None => "no playable format".into(),
    }
}

fn apply(media: &mut Media, info: PlayerInfo) {
    if let Some(title) = info.title {
        media.title = title;
    }
    if let Some(uploader) = info.uploader {
        media.uploader = Some(uploader);
    }
    if let Some(duration) = info.duration {
        media.duration = Some(duration);
    }
    media.formats = info.formats;
}

fn format_from(item: &Value, source: &'static str) -> Option<Format> {
    let itag = item.get("itag")?.as_u64()?;
    let mime = item.get("mimeType")?.as_str()?;
    let (container, codec) = parse_mime(mime);

    let has_audio = mime.starts_with("audio/")
        || item.get("audioQuality").is_some()
        || item.get("audioSampleRate").is_some();
    let has_video = mime.starts_with("video/") || item.get("width").is_some();
    let kind = match (has_video, has_audio) {
        (true, true) => StreamKind::Muxed,
        (true, false) => StreamKind::Video,
        (false, true) => StreamKind::Audio,
        (false, false) => return None,
    };

    Some(Format {
        id: itag.to_string(),
        kind,
        container,
        codec,
        width: uint(item, &["width"]).unwrap_or(0) as u32,
        height: uint(item, &["height"]).unwrap_or(0) as u32,
        fps: uint(item, &["fps"]).unwrap_or(0) as u32,
        bitrate: uint(item, &["bitrate"]).unwrap_or(0),
        bytes: uint(item, &["contentLength"]),
        url: item.get("url").and_then(|v| v.as_str()).map(str::to_string),
        headers: Vec::new(),
        source,
    })
}

fn parse_mime(mime: &str) -> (Container, String) {
    let container = if mime.starts_with("video/mp4") || mime.starts_with("audio/mp4") {
        Container::Mp4
    } else if mime.starts_with("video/webm") || mime.starts_with("audio/webm") {
        Container::Webm
    } else {
        Container::Unknown
    };
    let codec = match mime.find("codecs=\"") {
        Some(at) => {
            let rest = &mime[at + 8..];
            match rest.find('"') {
                Some(end) => rest[..end].to_string(),
                None => rest.to_string(),
            }
        }
        None => mime.split(';').next().unwrap_or("").trim().to_string(),
    };
    (container, codec)
}

fn video_id(url: &Url) -> Option<String> {
    if let Some(id) = url.query_param("v") {
        if plausible_id(&id) {
            return Some(id);
        }
    }
    let segments = url.segments();
    if url.host() == Some("youtu.be") || url.host() == Some("youtube-nocookie.com") {
        if let Some(first) = segments.first() {
            if plausible_id(first) {
                return Some((*first).to_string());
            }
        }
    }
    for (index, segment) in segments.iter().enumerate() {
        if matches!(*segment, "shorts" | "embed" | "live" | "v" | "e") {
            if let Some(next) = segments.get(index + 1) {
                if plausible_id(next) {
                    return Some((*next).to_string());
                }
            }
        }
    }
    None
}

/// A YouTube id is exactly eleven characters of `[A-Za-z0-9_-]`. Checking it
/// here means a typo is a usage error before a request is made, instead of a
/// 404 from a URL that was assembled from garbage.
fn plausible_id(id: &str) -> bool {
    id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Reads a `"key":"value"` pair out of the page's inline state, for the two
/// values that cannot be obtained any other way.
fn inline_string(page: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":\"");
    let start = page.find(&marker)? + marker.len();
    let rest = &page[start..];
    let end = rest.find('"')?;
    let value = &rest[..end];
    if value.is_empty() || value.contains('\\') {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(url: &str) -> Option<String> {
        video_id(&Url::parse(url).unwrap())
    }

    #[test]
    fn finds_the_id_in_every_url_shape() {
        assert_eq!(
            id("https://www.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            id("https://youtu.be/dQw4w9WgXcQ?t=10").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            id("https://www.youtube.com/shorts/dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            id("https://www.youtube.com/embed/dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            id("https://www.youtube.com/live/dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            id("https://music.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn rejects_an_id_that_cannot_be_one() {
        assert_eq!(id("https://www.youtube.com/watch?v=short"), None);
        assert_eq!(id("https://www.youtube.com/watch"), None);
        assert_eq!(id("https://www.youtube.com/playlist?list=PL123"), None);
    }

    #[test]
    fn parses_a_mime_type() {
        let (container, codec) = parse_mime("video/mp4; codecs=\"avc1.640028\"");
        assert_eq!(container, Container::Mp4);
        assert_eq!(codec, "avc1.640028");

        let (container, codec) = parse_mime("audio/webm; codecs=\"opus\"");
        assert_eq!(container, Container::Webm);
        assert_eq!(codec, "opus");

        let (container, _) = parse_mime("application/x-mpegURL");
        assert_eq!(container, Container::Unknown);
    }

    #[test]
    fn classifies_formats_by_what_they_carry() {
        let muxed = json!({
            "itag": 22,
            "mimeType": "video/mp4; codecs=\"avc1.64001F, mp4a.40.2\"",
            "width": 1280,
            "height": 720,
            "fps": 30,
            "bitrate": 1500000,
            "contentLength": "12000000",
            "url": "https://example/22",
            "audioQuality": "AUDIO_QUALITY_MEDIUM"
        });
        let format = format_from(&muxed, "test").unwrap();
        assert_eq!(format.kind, StreamKind::Muxed);
        assert_eq!(format.height, 720);
        assert_eq!(format.bytes, Some(12_000_000));

        let video_only = json!({
            "itag": 137,
            "mimeType": "video/mp4; codecs=\"avc1.640028\"",
            "width": 1920,
            "height": 1080,
            "bitrate": 4000000,
            "url": "https://example/137"
        });
        assert_eq!(
            format_from(&video_only, "test").unwrap().kind,
            StreamKind::Video
        );

        let audio_only = json!({
            "itag": 140,
            "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
            "bitrate": 128000,
            "url": "https://example/140"
        });
        assert_eq!(
            format_from(&audio_only, "test").unwrap().kind,
            StreamKind::Audio
        );
    }

    #[test]
    fn a_player_document_with_a_bot_check_produces_a_refusal() {
        let document = json!({
            "playabilityStatus": {
                "status": "LOGIN_REQUIRED",
                "reason": "Sign in to confirm you're not a bot"
            }
        });
        let info = read_player(&document, "test");
        assert!(info.formats.is_empty());
        assert!(describe(&info).contains("Sign in to confirm"));

        let error = refusal("dQw4w9WgXcQ", &[format!("web: {}", describe(&info))]);
        assert!(matches!(error, Error::Refused(_)));
        assert!(error.to_string().contains("sign-in check"));
    }

    #[test]
    fn a_ciphered_player_document_produces_its_own_refusal() {
        let document = json!({
            "playabilityStatus": {"status": "OK"},
            "streamingData": {
                "adaptiveFormats": [
                    {"itag": 137, "mimeType": "video/mp4", "signatureCipher": "s=abc&url=https%3A"}
                ]
            }
        });
        let info = read_player(&document, "test");
        assert_eq!(info.ciphered, 1);
        assert!(info.formats.is_empty());
        let error = refusal("dQw4w9WgXcQ", &[format!("android_vr: {}", describe(&info))]);
        assert!(error.to_string().contains("signature-protected"));
    }

    #[test]
    fn reads_an_inline_string() {
        let page = r#"<script>{"INNERTUBE_API_KEY":"KEY123","x":1}</script>"#;
        assert_eq!(
            inline_string(page, "INNERTUBE_API_KEY").as_deref(),
            Some("KEY123")
        );
        assert_eq!(inline_string(page, "MISSING"), None);
    }
}
