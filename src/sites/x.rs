//! X (Twitter).
//!
//! X serves posts to logged-out readers through the syndication endpoint that
//! its own embedded-tweet widget uses. That endpoint returns the post as JSON,
//! including `mediaDetails` with the MP4 variants of any video — already muxed,
//! so no remuxing is needed, and already free of the player's wrappers.
//!
//! The other route is HLS (`application/x-mpegURL`): a playlist of MPEG-TS
//! segments, which `hlsfetch` demuxes and writes as an MP4 in this process. It
//! is used when a post offers nothing else, and skipped when a progressive MP4
//! is available — one request instead of one per segment.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{Http, UA_DESKTOP};
use crate::model::{Container, Format, Image, Media, Site, StreamKind};
use crate::sites::{at, list, text, uint};
use crate::url::Url;

const HEADERS: [(&str, &str); 2] = [
    ("accept-language", "en-US,en;q=0.9"),
    ("referer", "https://x.com/"),
];

pub fn extract(http: &Http, url: &Url, raw: &str) -> Result<Media> {
    let id = status_id(url).ok_or_else(|| {
        Error::Usage(format!(
            "{raw} is not a post URL: expected https://x.com/<user>/status/<id>"
        ))
    })?;

    let page_url = format!("https://x.com/i/status/{id}");
    let mut media = Media::new(Site::X, id.clone(), page_url.clone());

    // `t.co` links are redirectors; the post id is only visible after following
    // one, which `status_id` cannot do on its own.
    let id = if id.is_empty() {
        let resolved = http.resolve(raw)?;
        status_id(&Url::parse(&resolved)?)
            .ok_or_else(|| Error::Usage(format!("{raw} does not lead to a post")))?
    } else {
        id
    };

    let endpoint =
        format!("https://cdn.syndication.twimg.com/tweet-result?id={id}&lang=en&token=tafas");
    let value = http.get_json_opt("x-syndication", &endpoint, &HEADERS)?;
    let value = match value {
        Some(value) => value,
        None => {
            return Err(Error::Unsupported(format!(
                "x: {id}: the post is gone, private, or too old for the syndication endpoint"
            )))
        }
    };

    let typename = text(&value, &["__typename"]).unwrap_or_default();
    if typename.contains("Tombstone") || typename.contains("Unavailable") {
        return Err(Error::Refused(format!(
            "x: {id}: the post is unavailable without an account (deleted, protected or \
             age-restricted); cookies from --cookies FILE are sent when you name a file"
        )));
    }

    media.title = text(&value, &["text"]).unwrap_or_default();
    media.uploader = text(&value, &["user", "screen_name"]);
    if media.title.is_empty() {
        media.title = format!("post {id}");
    }

    // Two shapes exist: `mediaDetails` (current) and a single `video` object
    // (older responses). Both are read; neither is required.
    let mut details: Vec<&Value> = list(&value, &["mediaDetails"]).iter().collect();
    if details.is_empty() {
        if let Some(video) = at(&value, &["video"]) {
            details.push(video);
        }
    }

    let request_headers = vec![
        ("user-agent".to_string(), UA_DESKTOP.to_string()),
        ("referer".to_string(), "https://x.com/".to_string()),
    ];

    for (index, detail) in details.iter().enumerate() {
        let kind = text(detail, &["type"]).unwrap_or_default();
        let is_video = detail.get("video_info").is_some() || kind == "video" || kind == "gif";
        if is_video {
            if let Some(duration_ms) = uint(detail, &["video_info", "duration_millis"]) {
                media.duration = Some(duration_ms / 1000);
            }
            for (variant_index, variant) in
                list(detail, &["video_info", "variants"]).iter().enumerate()
            {
                let content_type = text(variant, &["content_type"]).unwrap_or_default();
                let Some(variant_url) = text(variant, &["url"]) else {
                    continue;
                };
                if content_type.contains("mpegurl") {
                    // The playlist is fetched, demuxed and written as an MP4 in
                    // this process. It is a fallback rather than a first
                    // choice: a progressive MP4 is one request, a playlist is
                    // one per segment.
                    media.formats.push(Format {
                        id: format!("x-{}-{}", index + 1, variant_index + 1),
                        kind: StreamKind::Muxed,
                        container: Container::Hls,
                        codec: String::new(),
                        width: 0,
                        height: 0,
                        fps: 0,
                        bitrate: 0,
                        bytes: None,
                        url: Some(variant_url),
                        headers: request_headers.clone(),
                        source: "hls",
                    });
                    continue;
                }
                if !content_type.contains("mp4") {
                    continue;
                }
                media.formats.push(Format {
                    id: format!("x-{}-{}", index + 1, variant_index + 1),
                    kind: StreamKind::Muxed,
                    container: Container::Mp4,
                    codec: String::new(),
                    width: 0,
                    height: 0,
                    fps: 0,
                    bitrate: uint(variant, &["bit_rate"]).unwrap_or(0),
                    bytes: None,
                    url: Some(variant_url),
                    headers: request_headers.clone(),
                    source: "x-syndication",
                });
            }
        }

        // X names the poster of a video with the same field a photo uses, so a
        // video detail carries both the stream and its still.
        if is_video && detail.get("video_info").is_some() {
            if let Some(poster) = text(detail, &["media_url_https"]) {
                media.thumbnail = Some(Image {
                    url: poster,
                    extension: "jpg",
                    headers: request_headers.clone(),
                });
            }
        }

        if kind == "photo" {
            if let Some(image_url) = text(detail, &["media_url_https"]) {
                media.images.push(Image {
                    url: image_url,
                    extension: "jpg",
                    headers: request_headers.clone(),
                });
            }
        }
    }

    // A post that offers both shapes gets the progressive one: same picture,
    // fewer requests, and no demuxing to get wrong.
    if media
        .formats
        .iter()
        .any(|format| format.container == Container::Mp4)
    {
        media
            .formats
            .retain(|format| format.container != Container::Hls);
    }

    if media.formats.is_empty() && media.images.is_empty() {
        return Err(Error::Unsupported(format!(
            "x: {id}: the post carries no video or photo"
        )));
    }

    Ok(media)
}

fn status_id(url: &Url) -> Option<String> {
    let segments = url.segments();
    for (index, segment) in segments.iter().enumerate() {
        if matches!(*segment, "status" | "statuses") {
            if let Some(next) = segments.get(index + 1) {
                let cleaned = next.split('?').next().unwrap_or(next);
                if is_id(cleaned) {
                    return Some(cleaned.to_string());
                }
            }
        }
    }
    // t.co has no id in its path at all; the caller resolves it first.
    if url.host() == Some("t.co") {
        return Some(String::new());
    }
    None
}

fn is_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 25 && value.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(url: &str) -> Option<String> {
        status_id(&Url::parse(url).unwrap()).filter(|id| !id.is_empty())
    }

    #[test]
    fn finds_the_post_id() {
        assert_eq!(id("https://x.com/jack/status/20").as_deref(), Some("20"));
        assert_eq!(
            id("https://twitter.com/i/web/status/1585341984679469056").as_deref(),
            Some("1585341984679469056")
        );
        assert_eq!(
            id("https://mobile.twitter.com/u/statuses/123456").as_deref(),
            Some("123456")
        );
        assert_eq!(id("https://x.com/jack"), None);
    }

    #[test]
    fn t_co_is_marked_for_resolution() {
        let url = Url::parse("https://t.co/abc").unwrap();
        assert_eq!(status_id(&url).as_deref(), Some(""));
    }

    #[test]
    fn is_id_accepts_only_digits() {
        assert!(is_id("20"));
        assert!(!is_id("abc"));
        assert!(!is_id(""));
        assert!(!is_id("12345678901234567890123456"));
    }
}
