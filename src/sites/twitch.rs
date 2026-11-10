//! Twitch VODs.
//!
//! A recorded broadcast is served from `usher.ttvnw.net`, and that host wants a
//! token: `sig` and `nauth` come from GraphQL's `PlaybackAccessToken`, which the
//! site's own player asks for by persisted-query hash with the **public web
//! client id** in a header. The number that comes back is a JSON document that
//! has to be percent-encoded before it goes in a query string.
//!
//! One request is enough for everything: the batch carries the token operation
//! *and* a `video(id:)` query for the title, the uploader, the length and the
//! poster, so a capture does not pay a second round trip for a filename.
//!
//! A channel URL is not a VOD and is refused as such: a live stream has no end,
//! and a file needs one.
//!
//! A **clip** is a third thing, and a simpler one: `VideoAccessToken_Clip`
//! answers with one `sourceURL` per rendition, each of which is a plain MP4 with
//! picture and sound already joined. So a clip is the only route here that needs
//! no HLS fetch at all — the chosen URL is copied verbatim, and the container is
//! already what this program writes.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::extractor_args::ExtractorArgs;
use crate::http::Http;
use crate::model::{Container, Format, Image, Media, Site, StreamKind};
use crate::sites::{at, text, uint};
use crate::url::Url;

const GQL: &str = "https://gql.twitch.tv/gql";

/// Twitch's public web client id: embedded in every `twitch.tv` page, tied to no
/// account, unlocking nothing private. The GQL endpoint refuses a request that
/// names no client, and this is the one the site's own player names. It is not a
/// credential and there is nothing to rotate — the same reasoning as
/// `PUBLIC_WEB_API_KEY` in `SECURITY.md`.
pub const PUBLIC_WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";

/// The persisted-query hash the player uses for `PlaybackAccessToken`. Twitch
/// asks by hash rather than by query text; if it ever changes, the API answers an
/// error object, which is a refusal that quotes it rather than a silent nothing.
const PLAYBACK_TOKEN_HASH: &str = "0828119ded1c13477966434e15800ff57ddacf13ba1911c443297a175cb92a5";

pub fn extract(
    http: &Http,
    url: &Url,
    raw: &str,
    // This reader has no `--extractor-args` key; the parameter is uniform so
    // that the table of readers has one shape.
    _args: &ExtractorArgs,
) -> Result<Media> {
    if let Some(slug) = clip_slug(url) {
        return extract_clip(http, &slug);
    }
    let id = vod_id(url).ok_or_else(|| {
        if is_channel(url) {
            Error::Unsupported(format!(
                "{raw}: a channel is a live stream, and this build captures files; a recorded \
                 broadcast is https://www.twitch.tv/videos/<id>"
            ))
        } else {
            Error::Usage(format!(
                "{raw} is not a Twitch URL this build reads: expected \
                 https://www.twitch.tv/videos/<id> for a recording or \
                 https://clips.twitch.tv/<slug> for a clip"
            ))
        }
    })?;

    // One batch: the playback token, and the metadata that names the file.
    let body = json!([
        {
            "operationName": "PlaybackAccessToken",
            "variables": {
                "isLive": false,
                "login": "",
                "isVod": true,
                "vodID": &id,
                "playerType": "site"
            },
            "extensions": { "persistedQuery": { "version": 1, "sha256Hash": PLAYBACK_TOKEN_HASH } }
        },
        {
            "query": "query($id: ID!) { video(id: $id) { title, lengthSeconds, previewThumbnailURL, owner { displayName } } }",
            "variables": { "id": &id }
        }
    ]);
    let headers = [("client-id", PUBLIC_WEB_CLIENT_ID)];
    let answer = http.post_json("twitch-gql", GQL, &headers, &body)?;
    parse(&answer, &id)
}

/// The clip query, as the site's own player writes it.
///
/// `sourceURL` comes back *without* its query string, and the token and
/// signature are appended by the caller — the same `sig`/`token` pair the player
/// adds, in that order. A clip whose transcoding has not finished answers with
/// an empty `sourceURL`, which is a refusal rather than a broken file.
const CLIP_QUERY: &str = "query VideoAccessToken_Clip($slug: ID!) {\n  clip(slug: $slug) {\n    id\n    title\n    durationSeconds\n    broadcaster { displayName }\n    thumbnailURL\n    videoQualities { frameRate quality sourceURL }\n    playbackAccessToken(params: {platform: \"web\", playerBackend: \"mediaplayer\", playerType: \"site\"}) { signature value }\n  }\n}";

fn extract_clip(http: &Http, slug: &str) -> Result<Media> {
    let body = json!({
        "operationName": "VideoAccessToken_Clip",
        "query": CLIP_QUERY,
        "variables": { "slug": slug }
    });
    let headers = [("client-id", PUBLIC_WEB_CLIENT_ID)];
    let answer = http.post_json("twitch-clip", GQL, &headers, &body)?;
    parse_clip(&answer, slug)
}

/// One clip, read into a [`Media`]: every rendition the API listed, each with
/// the token that makes its URL work.
///
/// The renditions are ordinary MP4s with the audio already in them, so the
/// selection needs no remux and the file arrives as it was served.
pub fn parse_clip(answer: &Value, slug: &str) -> Result<Media> {
    if let Some(message) = first_error(answer) {
        return Err(Error::Refused(format!(
            "twitch: clip {slug}: the API refused the query: {message}"
        )));
    }
    let Some(clip) = answer
        .get("data")
        .and_then(|data| at(data, &["clip"]))
        .filter(|clip| !clip.is_null())
    else {
        return Err(Error::Refused(format!(
            "twitch: clip {slug}: there is no such clip — the slug is wrong, or the clip was \
             deleted. A clip URL looks like https://clips.twitch.tv/<Slug> or \
             https://www.twitch.tv/<channel>/clip/<Slug>"
        )));
    };

    let signature = at(clip, &["playbackAccessToken"])
        .and_then(|token| text(token, &["signature"]))
        .ok_or_else(|| {
            Error::Refused(format!(
                "twitch: clip {slug}: the API returned no playback signature"
            ))
        })?;
    let token = at(clip, &["playbackAccessToken"])
        .and_then(|token| text(token, &["value"]))
        .ok_or_else(|| {
            Error::Refused(format!(
                "twitch: clip {slug}: the API returned no playback token"
            ))
        })?;

    let mut media = Media::new(
        Site::Twitch,
        clip_id(clip, slug),
        format!("https://clips.twitch.tv/{slug}"),
    );
    media.title = text(clip, &["title"]).unwrap_or_default();
    media.uploader = text(clip, &["broadcaster", "displayName"]);
    media.duration = uint(clip, &["durationSeconds"]).filter(|seconds| *seconds > 0);
    media.thumbnail = text(clip, &["thumbnailURL"])
        .filter(|url| !url.is_empty())
        .map(|url| Image {
            url,
            extension: "jpg",
            headers: request_headers(),
        });
    if media.title.is_empty() {
        media.title = format!("twitch clip {slug}");
    }

    let qualities = clip
        .get("videoQualities")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            Error::Refused(format!(
                "twitch: clip {slug}: the API listed no video qualities"
            ))
        })?;
    for quality in qualities {
        // An empty `sourceURL` means "this rendition is still being encoded".
        // Listing it would put a choice in front of the user that cannot be
        // fetched, so it is skipped rather than offered and then refused.
        let Some(source) = text(quality, &["sourceURL"]).filter(|url| !url.is_empty()) else {
            continue;
        };
        let label = text(quality, &["quality"]).unwrap_or_default();
        media.formats.push(Format {
            id: if label.is_empty() {
                "clip".to_string()
            } else {
                label.clone()
            },
            kind: StreamKind::Muxed,
            container: Container::Mp4,
            codec: String::new(),
            width: 0,
            height: label.parse().unwrap_or(0),
            fps: uint(quality, &["frameRate"]).unwrap_or(0) as u32,
            bitrate: 0,
            bytes: None,
            url: Some(clip_url(&source, &signature, &token)),
            headers: request_headers(),
            source: "twitch-clip",
        });
    }
    if media.formats.is_empty() {
        return Err(Error::Refused(format!(
            "twitch: clip {slug}: every rendition is still being encoded (the clip's \
             \"creation state\" has not reached CREATED); try again in a minute"
        )));
    }
    // Best resolution first, which is the order a user expects `--quality best`
    // to mean. `quality` is a string like "1080", so it is parsed as a height.
    media
        .formats
        .sort_by(|a, b| b.height.cmp(&a.height).then_with(|| b.fps.cmp(&a.fps)));
    Ok(media)
}

/// `sourceURL` + `?sig=` + `&token=`, in that order: the signature is sent as it
/// arrived and the token is percent-encoded, exactly as the `nauth` case above.
fn clip_url(source: &str, signature: &str, token: &str) -> String {
    let separator = if source.contains('?') { '&' } else { '?' };
    format!(
        "{source}{separator}sig={signature}&token={}",
        percent_encode(token)
    )
}

/// The clip's own id when the API gave one, so two clips with the same title do
/// not collide on disk.
fn clip_id(clip: &Value, slug: &str) -> String {
    text(clip, &["id"]).unwrap_or_else(|| slug.to_string())
}

/// The batch, read into a [`Media`]: the token becomes the playlist URL, the
/// `video` query becomes the name.
pub fn parse(answer: &Value, id: &str) -> Result<Media> {
    let entries = answer.as_array().ok_or_else(|| {
        Error::Refused(format!(
            "twitch: {id}: the API answered something that is not a batch of operations"
        ))
    })?;

    if let Some(message) = entries.iter().find_map(first_error) {
        return Err(Error::Refused(format!(
            "twitch: {id}: the API refused the query: {message}"
        )));
    }

    let token = entries
        .first()
        .and_then(|entry| at(entry, &["data", "streamPlaybackAccessToken"]))
        .filter(|token| !token.is_null());
    let Some(token) = token else {
        return Err(Error::Refused(format!(
            "twitch: {id}: there is no playback token — the recording has expired, was deleted, \
             or needs a subscription; --cookies FILE is sent when you name a file, and no \
             challenge is solved"
        )));
    };
    let value = text(token, &["value"]).ok_or_else(|| {
        Error::Refused(format!("twitch: {id}: the playback token carries no value"))
    })?;
    let signature = text(token, &["signature"]).ok_or_else(|| {
        Error::Refused(format!(
            "twitch: {id}: the playback token carries no signature"
        ))
    })?;

    let mut media = Media::new(
        Site::Twitch,
        id.to_string(),
        format!("https://www.twitch.tv/videos/{id}"),
    );
    if let Some(video) = entries
        .get(1)
        .and_then(|entry| at(entry, &["data", "video"]))
        .filter(|video| !video.is_null())
    {
        media.title = text(video, &["title"]).unwrap_or_default();
        media.uploader = text(video, &["owner", "displayName"]);
        media.duration = uint(video, &["lengthSeconds"]).filter(|seconds| *seconds > 0);
        media.thumbnail = text(video, &["previewThumbnailURL"]).map(|url| Image {
            url,
            extension: "jpg",
            headers: request_headers(),
        });
    }
    if media.title.is_empty() {
        media.title = format!("twitch video {id}");
    }

    media.formats.push(Format {
        id: "twitch-vod".to_string(),
        kind: StreamKind::Muxed,
        container: Container::Hls,
        codec: String::new(),
        width: 0,
        height: 0,
        fps: 0,
        bitrate: 0,
        bytes: None,
        url: Some(usher_url(id, &value, &signature)),
        headers: request_headers(),
        source: "twitch-usher",
    });
    Ok(media)
}

fn first_error(entry: &Value) -> Option<String> {
    entry
        .get("errors")?
        .as_array()?
        .first()
        .and_then(|error| text(error, &["message"]))
}

/// The playlist the recording is served from.
///
/// `nauth` is the token, and the token is a JSON document — quotes, braces and
/// backslashes — so it is percent-encoded. `nauthsig` is a signature over it and
/// must be sent as it arrived.
fn usher_url(id: &str, token: &str, signature: &str) -> String {
    format!(
        "https://usher.ttvnw.net/vod/{id}.m3u8?allow_source=true&allow_audio_only=true\
         &fast_bread=true&player=twitchweb&playlist_include_framerate=true&nauth={}&nauthsig={signature}",
        percent_encode(token)
    )
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_' | b'~') {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

fn request_headers() -> Vec<(String, String)> {
    vec![
        (
            "user-agent".to_string(),
            crate::http::UA_DESKTOP.to_string(),
        ),
        ("client-id".to_string(), PUBLIC_WEB_CLIENT_ID.to_string()),
    ]
}

/// `/videos/<id>` and nothing else. `/videos` is what every VOD URL has in common.
fn vod_id(url: &Url) -> Option<String> {
    let segments = url.segments();
    let index = segments.iter().position(|segment| *segment == "videos")?;
    segments
        .get(index + 1)
        .filter(|id| is_id(id))
        .map(|id| (*id).to_string())
}

/// The slug of a clip URL, in the four shapes Twitch uses:
///
/// * `https://clips.twitch.tv/<Slug>`
/// * `https://clips.twitch.tv/embed?clip=<Slug>`
/// * `https://www.twitch.tv/<channel>/clip/<Slug>`
/// * `https://m.twitch.tv/<channel>/clip/<Slug>`
///
/// A slug is case-sensitive and mixes both cases, which is why it is not
/// validated character by character the way a numeric VOD id is.
fn clip_slug(url: &Url) -> Option<String> {
    let segments = url.segments();
    if url.host() == Some("clips.twitch.tv") {
        // The embed form puts the slug in the query string, not the path, and
        // `query_param` already percent-decodes it.
        if let Some(slug) = url.query_param("clip").filter(|slug| is_slug(slug)) {
            return Some(slug);
        }
        return segments
            .iter()
            .find(|segment| is_slug(segment))
            .map(|slug| (*slug).to_string());
    }
    let index = segments
        .iter()
        .position(|segment| *segment == "clip" || *segment == "clips")?;
    segments
        .get(index + 1)
        .filter(|slug| is_slug(slug))
        .map(|slug| (*slug).to_string())
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_channel(url: &Url) -> bool {
    let segments = url.segments();
    segments.len() == 1 && !is_id(segments[0])
}

fn is_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 12 && value.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer() -> Value {
        json!([
            {
                "data": {
                    "streamPlaybackAccessToken": {
                        "value": "{\"authorization\":{\"forbidden\":false},\"channels\":{}}",
                        "signature": "abc123signature"
                    }
                }
            },
            {
                "data": {
                    "video": {
                        "title": "A long night of chess",
                        "lengthSeconds": 7200,
                        "previewThumbnailURL": "https://static-cdn.jtvnw.net/cf_vods/thumb.jpg",
                        "owner": { "displayName": "someone" }
                    }
                }
            }
        ])
    }

    #[test]
    fn a_recording_becomes_one_playlist_with_a_token() {
        let media = parse(&answer(), "1234567890").expect("a recording parses");
        assert_eq!(media.site, Site::Twitch);
        assert_eq!(media.title, "A long night of chess");
        assert_eq!(media.uploader.as_deref(), Some("someone"));
        assert_eq!(media.duration, Some(7200));
        assert_eq!(media.formats.len(), 1);
        assert_eq!(media.formats[0].container, Container::Hls);
        let url = media.formats[0].url.as_deref().unwrap();
        assert!(
            url.starts_with("https://usher.ttvnw.net/vod/1234567890.m3u8?"),
            "{url}"
        );
        assert!(url.contains("nauthsig=abc123signature"), "{url}");
        assert!(
            url.contains("nauth=%7B%22authorization%22"),
            "the token is percent-encoded: {url}"
        );
        assert!(
            !url.contains('{'),
            "no raw braces may reach the query string: {url}"
        );
    }

    #[test]
    fn a_subscriber_only_recording_is_refused_by_name() {
        let answer = json!([{ "data": { "streamPlaybackAccessToken": null } }, { "data": { "video": null } }]);
        let error = parse(&answer, "1").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("subscription"), "{text}");
        assert!(text.contains("--cookies"), "{text}");
    }

    #[test]
    fn an_api_error_is_quoted_rather_than_swallowed() {
        let answer = json!([{ "errors": [{ "message": "service timeout" }] }]);
        let error = parse(&answer, "1").unwrap_err();
        assert!(error.to_string().contains("service timeout"), "{error}");
    }

    #[test]
    fn percent_encoding_keeps_only_the_unreserved_characters() {
        assert_eq!(percent_encode("abcXYZ019-._~"), "abcXYZ019-._~");
        assert_eq!(percent_encode("{\"a\":\"b\"}"), "%7B%22a%22%3A%22b%22%7D");
        assert_eq!(percent_encode("a b"), "a%20b");
    }

    fn clip_answer() -> Value {
        json!({ "data": { "clip": {
            "id": "DelightfulFaintCockroachKeepo",
            "title": "A one-in-a-million shot",
            "durationSeconds": 27,
            "broadcaster": { "displayName": "someone" },
            "thumbnailURL": "https://clips-media-assets2.twitch.tv/thumb.jpg",
            "videoQualities": [
                { "frameRate": 30, "quality": "360",
                  "sourceURL": "https://clips-media-assets2.twitch.tv/360.mp4" },
                { "frameRate": 60, "quality": "1080",
                  "sourceURL": "https://clips-media-assets2.twitch.tv/1080.mp4" },
                { "frameRate": 30, "quality": "720", "sourceURL": "" }
            ],
            "playbackAccessToken": {
                "signature": "abc123signature",
                "value": "{\"authorization\":{\"forbidden\":false}}"
            }
        } } })
    }

    #[test]
    fn a_clip_becomes_one_mp4_per_remarkable_rendition() {
        let media = parse_clip(&clip_answer(), "SomeSlug").expect("a clip parses");
        assert_eq!(media.site, Site::Twitch);
        assert_eq!(media.title, "A one-in-a-million shot");
        assert_eq!(media.uploader.as_deref(), Some("someone"));
        assert_eq!(media.duration, Some(27));
        assert_eq!(media.id, "DelightfulFaintCockroachKeepo");
        // Three renditions were listed and one of them is still encoding, so
        // two are offered: the empty `sourceURL` is skipped, not offered and
        // then refused.
        assert_eq!(media.formats.len(), 2);
        assert_eq!(media.formats[0].height, 1080, "best first");
        assert_eq!(media.formats[1].height, 360);
        for format in &media.formats {
            assert_eq!(format.container, Container::Mp4, "a clip is already an MP4");
            assert_eq!(format.kind, StreamKind::Muxed, "sound is inside it");
        }
        let url = media.formats[0].url.as_deref().unwrap();
        assert!(
            url.starts_with("https://clips-media-assets2.twitch.tv/1080.mp4?"),
            "{url}"
        );
        assert!(url.contains("sig=abc123signature"), "{url}");
        assert!(
            url.contains("&token=%7B%22authorization%22"),
            "the token is percent-encoded: {url}"
        );
    }

    #[test]
    fn a_clip_that_is_still_encoding_is_refused_rather_than_written_empty() {
        let answer = json!({ "data": { "clip": {
            "id": "x", "title": "t", "durationSeconds": 5,
            "videoQualities": [{ "frameRate": 30, "quality": "1080", "sourceURL": "" }],
            "playbackAccessToken": { "signature": "s", "value": "v" }
        } } });
        let error = parse_clip(&answer, "SomeSlug").unwrap_err();
        assert!(error.to_string().contains("being encoded"), "{error}");
    }

    #[test]
    fn a_missing_clip_names_the_shapes_that_would_work() {
        let answer = json!({ "data": { "clip": null } });
        let error = parse_clip(&answer, "NoSuchClip").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("clips.twitch.tv"), "{text}");
        assert!(text.contains("/clip/"), "{text}");
    }

    #[test]
    fn clip_urls_in_every_shape_the_site_uses() {
        let url = |raw: &str| Url::parse(raw).unwrap();
        assert_eq!(
            clip_slug(&url(
                "https://clips.twitch.tv/AwkwardHelplessSalamanderSwiftRage"
            )),
            Some("AwkwardHelplessSalamanderSwiftRage".into())
        );
        assert_eq!(
            clip_slug(&url(
                "https://clips.twitch.tv/embed?clip=AwkwardHelplessSalamanderSwiftRage&parent=x"
            )),
            Some("AwkwardHelplessSalamanderSwiftRage".into()),
            "the embed form keeps the slug in the query"
        );
        assert_eq!(
            clip_slug(&url("https://www.twitch.tv/someone/clip/AwkwardHelpless")),
            Some("AwkwardHelpless".into())
        );
        assert_eq!(
            clip_slug(&url("https://m.twitch.tv/someone/clip/AwkwardHelpless")),
            Some("AwkwardHelpless".into())
        );
        assert_eq!(
            clip_slug(&url("https://www.twitch.tv/videos/1234567890")),
            None
        );
        assert_eq!(clip_slug(&url("https://www.twitch.tv/someone")), None);
    }

    #[test]
    fn ids_and_channels() {
        let url = |raw: &str| Url::parse(raw).unwrap();
        assert_eq!(
            vod_id(&url("https://www.twitch.tv/videos/1234567890")),
            Some("1234567890".into())
        );
        assert_eq!(
            vod_id(&url("https://m.twitch.tv/videos/42")),
            Some("42".into())
        );
        assert_eq!(vod_id(&url("https://www.twitch.tv/someone")), None);
        assert!(is_channel(&url("https://www.twitch.tv/someone")));
        assert!(!is_channel(&url("https://www.twitch.tv/videos/42")));
    }
}
