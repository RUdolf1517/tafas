//! Reddit.
//!
//! A post that carries a video is read from its own JSON: `/comments/<id>.json`,
//! the listing the old mobile clients asked for, whose first child is the post
//! and whose `secure_media.reddit_video` is the video. From that object exactly
//! one route is taken — `hls_url` — and the reason is worth stating, because two
//! more obvious ones exist and neither is offered:
//!
//! * `fallback_url` (`DASH_720.mp4`, and the other heights beside it) is a
//!   **video-only** MP4. Writing it as a finished video would be a file with a
//!   picture and no sound, which is the kind of quiet lie this program refuses.
//! * `dash_url` is a DASH manifest (`SegmentBase`), which this build does not
//!   read at all.
//!
//! The HLS playlist, by contrast, is a master list with the video renditions and
//! an `EXT-X-MEDIA` audio rendition, which `hlsfetch` follows and joins into one
//! MP4. A bare `v.redd.it/<hash>` needs no JSON at all: the playlist lives at a
//! fixed path under the same hash, so the URL is built rather than looked up.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Container, Format, Image, Media, Site, StreamKind};
use crate::sites::{at, text, uint};
use crate::url::Url;

const HEADERS: [(&str, &str); 1] = [("accept", "application/json")];

pub fn extract(http: &Http, url: &Url, raw: &str) -> Result<Media> {
    let host = url.host().unwrap_or_default();

    // `v.redd.it/<hash>` names the media, not a post: the playlist is at a fixed
    // path, so there is nothing to look up.
    if host == "v.redd.it" {
        let hash = media_hash(url).ok_or_else(|| {
            Error::Usage(format!(
                "{raw} is not a media URL: expected https://v.redd.it/<hash>"
            ))
        })?;
        return Ok(playlist_media(&hash, format!("https://v.redd.it/{hash}")));
    }

    let id = post_id(url).ok_or_else(|| {
        Error::Usage(format!(
            "{raw} is not a post URL: expected https://www.reddit.com/r/<sub>/comments/<id>/… \
             or https://redd.it/<id>"
        ))
    })?;

    let endpoint = format!("https://www.reddit.com/comments/{id}.json?raw_json=1");
    let value = http.get_json_opt("reddit-post", &endpoint, &HEADERS)?;
    let Some(value) = value else {
        return Err(Error::Refused(format!(
            "reddit: {id}: the post answers 404 — it is deleted, private, or in a community that \
             does not exist any more"
        )));
    };
    parse(&value, &id, format!("https://www.reddit.com/comments/{id}"))
}

/// The listing, read into a [`Media`]. One route: the HLS playlist.
pub fn parse(value: &Value, id: &str, page_url: String) -> Result<Media> {
    let post = post_of(value)
        .ok_or_else(|| Error::Refused(format!("reddit: {id}: the answer carries no post")))?;

    if let Some(reason) = text(post, &["removed_by_category"]) {
        return Err(Error::Refused(format!(
            "reddit: {id}: the post was removed ({reason}); what is left of it carries no video"
        )));
    }

    let video = at(post, &["secure_media", "reddit_video"])
        .or_else(|| at(post, &["media", "reddit_video"]));

    let (Some(video), Some(hls)) = (video, video.and_then(|video| text(video, &["hls_url"])))
    else {
        return Err(refusal(post, id));
    };

    let mut media = Media::new(Site::Reddit, id.to_string(), page_url);
    media.title = text(post, &["title"]).unwrap_or_default();
    if media.title.is_empty() {
        media.title = format!("reddit {id}");
    }
    media.uploader = text(post, &["author"]).filter(|author| author != "[deleted]");
    media.duration = uint(video, &["duration"]).filter(|seconds| *seconds > 0);
    media.thumbnail = thumbnail(post);

    media.formats.push(Format {
        id: "reddit-hls".to_string(),
        kind: StreamKind::Muxed,
        container: Container::Hls,
        codec: String::new(),
        width: uint(video, &["width"]).unwrap_or(0) as u32,
        height: uint(video, &["height"]).unwrap_or(0) as u32,
        fps: uint(video, &["fps"]).unwrap_or(0) as u32,
        bitrate: 0,
        bytes: None,
        url: Some(hls),
        headers: request_headers(),
        source: "reddit-hls",
    });

    Ok(media)
}

/// Why a post that has no playlist cannot be captured, as one sentence.
fn refusal(post: &Value, id: &str) -> Error {
    if post.get("gallery_data").is_some()
        || text(post, &["post_hint"]).is_some_and(|hint| hint.contains("image"))
    {
        return Error::Refused(format!(
            "reddit: {id}: the post is a gallery or a photo; an image is not a video, and images \
             are only written for posts that carry them as media"
        ));
    }
    if post.get("reddit_video").is_some()
        || at(post, &["secure_media", "reddit_video"]).is_some()
        || at(post, &["media", "reddit_video"]).is_some()
    {
        return Error::Refused(format!(
            "reddit: {id}: the video lists no HLS playlist, and the progressive files it does list \
             carry no audio (DASH_<height>.mp4), so it is refused rather than written silent"
        ));
    }
    if parse_bool(post, &["over_18"]) {
        return Error::Refused(format!(
            "reddit: {id}: the post carries no video and is marked over-18; --cookies FILE is sent \
             when you name a file, and no challenge is solved"
        ));
    }
    Error::Refused(format!(
        "reddit: {id}: the post carries no video — a text or link post has nothing to write"
    ))
}

fn parse_bool(value: &Value, path: &[&str]) -> bool {
    at(value, path).and_then(Value::as_bool).unwrap_or(false)
}

/// The post object inside a listing, whichever listing shape answered.
fn post_of(value: &Value) -> Option<&Value> {
    if let Some(child) = value
        .as_array()
        .and_then(|listings| listings.first())
        .and_then(|listing| listing.get("data"))
        .and_then(|data| data.get("children"))
        .and_then(|children| children.as_array())
        .and_then(|children| children.first())
        .and_then(|child| child.get("data"))
    {
        return Some(child);
    }
    // `/api/info`-shaped answers carry the post itself.
    if value.get("title").is_some() {
        return Some(value);
    }
    None
}

/// The poster. Reddit HTML-escapes the `&` in its preview URLs, so it is undone
/// rather than passed on as a query parameter called `amp;url`.
fn thumbnail(post: &Value) -> Option<Image> {
    let url = at(post, &["preview", "images"])
        .and_then(|images| images.as_array())
        .and_then(|images| images.first())
        .and_then(|image| at(image, &["source", "url"]))
        .and_then(Value::as_str)?
        .replace("&amp;", "&");
    Some(Image {
        url,
        extension: "jpg",
        headers: request_headers(),
    })
}

fn playlist_media(hash: &str, page_url: String) -> Media {
    let mut media = Media::new(Site::Reddit, hash.to_string(), page_url);
    media.title = format!("reddit {hash}");
    media.formats.push(Format {
        id: "reddit-hls".to_string(),
        kind: StreamKind::Muxed,
        container: Container::Hls,
        codec: String::new(),
        width: 0,
        height: 0,
        fps: 0,
        bitrate: 0,
        bytes: None,
        url: Some(format!("https://v.redd.it/{hash}/HLSPlaylist.m3u8")),
        headers: request_headers(),
        source: "reddit-hls",
    });
    media
}

fn request_headers() -> Vec<(String, String)> {
    vec![(
        "user-agent".to_string(),
        crate::http::UA_DESKTOP.to_string(),
    )]
}

/// The post id: the segment after `comments`, or the only segment of `redd.it`.
fn post_id(url: &Url) -> Option<String> {
    let segments = url.segments();
    if url.host() == Some("redd.it") {
        return segments
            .first()
            .filter(|id| is_id(id))
            .map(|id| id.to_string());
    }
    let at_comments = segments.iter().position(|segment| *segment == "comments")?;
    segments
        .get(at_comments + 1)
        .filter(|id| is_id(id))
        .map(|id| id.to_string())
}

fn media_hash(url: &Url) -> Option<String> {
    url.segments()
        .first()
        .filter(|hash| is_id(hash))
        .map(|hash| hash.to_string())
}

/// Reddit ids are base 36: letters and digits, no punctuation.
fn is_id(value: &str) -> bool {
    (4..=20).contains(&value.len()) && value.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn listing(post: Value) -> Value {
        json!([
            { "kind": "Listing", "data": { "children": [ { "kind": "t3", "data": post } ] } },
            { "kind": "Listing", "data": { "children": [] } }
        ])
    }

    fn video_post() -> Value {
        json!({
            "title": "A dog on a skateboard",
            "author": "someone",
            "over_18": false,
            "preview": { "images": [ { "source": {
                "url": "https://preview.redd.it/x.jpg?width=1080&amp;format=pjpg"
            } } ] },
            "secure_media": { "reddit_video": {
                "hls_url": "https://v.redd.it/abc123def45/HLSPlaylist.m3u8?source=fallback",
                "fallback_url": "https://v.redd.it/abc123def45/DASH_720.mp4?source=fallback",
                "dash_url": "https://v.redd.it/abc123def45/DASHPlaylist.mpd",
                "width": 1080,
                "height": 720,
                "fps": 30,
                "duration": 42
            } }
        })
    }

    #[test]
    fn a_video_post_becomes_one_hls_format() {
        let media = parse(
            &listing(video_post()),
            "abc123def45",
            "https://www.reddit.com/comments/abc123def45".into(),
        )
        .expect("a video post parses");
        assert_eq!(media.site, Site::Reddit);
        assert_eq!(media.title, "A dog on a skateboard");
        assert_eq!(media.uploader.as_deref(), Some("someone"));
        assert_eq!(media.duration, Some(42));
        assert_eq!(media.formats.len(), 1, "one route: the playlist");
        assert_eq!(media.formats[0].container, Container::Hls);
        assert_eq!(media.formats[0].height, 720);
        assert_eq!(
            media.thumbnail.as_ref().map(|image| image.url.as_str()),
            Some("https://preview.redd.it/x.jpg?width=1080&format=pjpg"),
            "the escaped ampersand is undone"
        );
    }

    #[test]
    fn a_video_without_a_playlist_is_refused_rather_than_written_silent() {
        let mut post = video_post();
        post["secure_media"]["reddit_video"]
            .as_object_mut()
            .unwrap()
            .remove("hls_url");
        let error = parse(&listing(post), "abc123def45", "https://x/".into()).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("no audio"), "{text}");
        assert!(text.contains("DASH_"), "{text}");
    }

    #[test]
    fn a_post_with_no_video_is_refused() {
        let post = json!({ "title": "just a picture", "post_hint": "image", "over_18": false });
        let error = parse(&listing(post), "abc123def45", "https://x/".into()).unwrap_err();
        assert!(error.to_string().contains("not a video"), "{error}");
    }

    #[test]
    fn a_removed_post_is_refused_by_name() {
        let post = json!({ "removed_by_category": "moderator" });
        let error = parse(&listing(post), "abc123def45", "https://x/".into()).unwrap_err();
        assert!(error.to_string().contains("moderator"), "{error}");
    }

    #[test]
    fn a_bare_media_url_needs_no_json() {
        let url = Url::parse("https://v.redd.it/abc123def45").unwrap();
        let hash = media_hash(&url).unwrap();
        let media = playlist_media(&hash, "https://v.redd.it/abc123def45".into());
        assert_eq!(
            media.formats[0].url.as_deref(),
            Some("https://v.redd.it/abc123def45/HLSPlaylist.m3u8")
        );
    }

    #[test]
    fn ids_come_from_both_url_shapes() {
        let id = |url: &str| post_id(&Url::parse(url).unwrap());
        assert_eq!(
            id("https://www.reddit.com/r/aww/comments/abc123def45/a_dog/"),
            Some("abc123def45".into())
        );
        assert_eq!(
            id("https://old.reddit.com/comments/abc123def45"),
            Some("abc123def45".into())
        );
        assert_eq!(
            id("https://redd.it/abc123def45"),
            Some("abc123def45".into())
        );
        assert_eq!(id("https://www.reddit.com/r/aww/"), None);
    }
}
