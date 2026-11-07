//! TikTok.
//!
//! There is no public API, so this reads the same document the web player
//! reads: the `__UNIVERSAL_DATA_FOR_REHYDRATION__` JSON that the video page
//! inlines. When the web page comes back as a verification wall, a second route
//! exists — the mobile feed endpoint, which is also what the phone app uses —
//! and it is tried before giving up.
//!
//! Unlike YouTube, TikTok hands out one progressive MP4 that already contains
//! audio, so a capture usually needs no remuxing at all.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::extractor_args::ExtractorArgs;
use crate::http::{Http, UA_DESKTOP};
use crate::model::{Container, Format, Image, Media, Site, StreamKind};
use crate::sites::{at, json_after, list, text, uint};
use crate::url::Url;

const PAGE_HEADERS: [(&str, &str); 2] = [
    ("accept-language", "en-US,en;q=0.9"),
    ("accept", "text/html,application/xhtml+xml"),
];

/// The app user agent is what makes the mobile feed answer with JSON rather
/// than with a verification page.
const UA_APP: &str = "com.ss.android.ugc.trill/190500 (Linux; U; Android 13; en; Pixel 7; \
                      Build/TQ3A.230805.001)";
const UA_APP_HEADERS_NAME: &str = "com.ss.android.ugc.trill";

pub fn extract(http: &Http, url: &Url, raw: &str, args: &ExtractorArgs) -> Result<Media> {
    let target = resolve(http, url, raw)?;
    let target_url = Url::parse(&target)?;
    let id = aweme_id(&target_url).ok_or_else(|| {
        Error::Usage(format!(
            "{raw} is not a TikTok video URL: expected /@user/video/ID or a vm.tiktok.com link"
        ))
    })?;

    let canonical = format!("https://www.tiktok.com{}", target_url.path);
    let headers: Vec<(String, String)> = vec![
        ("user-agent".to_string(), UA_DESKTOP.to_string()),
        ("referer".to_string(), "https://www.tiktok.com/".to_string()),
    ];
    let mut notes: Vec<String> = Vec::new();

    // `tiktok:route=` narrows the two routes. Both are on by default and the
    // second is the fallback, so a route that was excluded by hand is worth a
    // note rather than a silence: it is the difference between "the site said
    // no twice" and "you told me to ask once".
    let route = args.get("tiktok", "route");

    if route != Some("app") {
        match http.get_text(&canonical, &PAGE_HEADERS) {
            Ok(page) => {
                if let Some(document) = json_after(&page, "__UNIVERSAL_DATA_FOR_REHYDRATION__") {
                    match serde_json::from_str::<Value>(document) {
                        Ok(value) => {
                            if let Some(item) = at(
                                &value,
                                &[
                                    "__DEFAULT_SCOPE__",
                                    "webapp.video-detail",
                                    "itemInfo",
                                    "itemStruct",
                                ],
                            ) {
                                let media = from_item(item, &id, &canonical, &headers);
                                if !media.formats.is_empty() || !media.images.is_empty() {
                                    return Ok(media);
                                }
                                notes.push("web page: the item has no media URL".into());
                            } else {
                                let status = text(
                                    &value,
                                    &["__DEFAULT_SCOPE__", "webapp.video-detail", "statusMsg"],
                                )
                                .unwrap_or_else(|| "no itemInfo in the page".into());
                                notes.push(format!("web page: {status}"));
                            }
                        }
                        Err(error) => notes
                            .push(format!("web page: the inline data did not parse ({error})")),
                    }
                } else {
                    notes.push("web page: no inline state (verification wall?)".into());
                }
            }
            Err(error) => notes.push(format!("web page: {error}")),
        }
    } else {
        notes.push(
            "web page: not asked, --extractor-args pinned tiktok:route=app".to_string(),
        );
    }

    if route == Some("web") {
        notes.push(
            "mobile feed: not asked, --extractor-args pinned tiktok:route=web".to_string(),
        );
        return Err(Error::Refused(format!(
            "tiktok: {id} could not be read from the web page: {}",
            notes.join("; ")
        )));
    }

    match mobile_feed(http, &id, &canonical, &headers) {
        Ok(media) => Ok(media),
        Err(error) => {
            notes.push(format!("mobile feed: {error}"));
            Err(Error::Refused(format!(
                "tiktok: {id} could not be read: {}",
                notes.join("; ")
            )))
        }
    }
}

fn resolve(http: &Http, url: &Url, raw: &str) -> Result<String> {
    let host = url.host().unwrap_or("");
    if host == "vm.tiktok.com" || host == "vt.tiktok.com" {
        http.resolve(raw)
    } else {
        Ok(raw.to_string())
    }
}

fn aweme_id(url: &Url) -> Option<String> {
    if let Some(id) = url.query_param("item_id") {
        if digits(&id) {
            return Some(id);
        }
    }
    let segments = url.segments();
    for (index, segment) in segments.iter().enumerate() {
        if matches!(*segment, "video" | "photo") {
            if let Some(next) = segments.get(index + 1) {
                if digits(next) {
                    return Some((*next).to_string());
                }
            }
        }
    }
    segments
        .last()
        .filter(|s| digits(s))
        .map(|s| (*s).to_string())
}

fn digits(value: &str) -> bool {
    !value.is_empty() && value.len() <= 25 && value.chars().all(|c| c.is_ascii_digit())
}

/// The mobile feed: one GET, one JSON object, same fields as the page under
/// different names.
fn mobile_feed(http: &Http, id: &str, page: &str, headers: &[(String, String)]) -> Result<Media> {
    let endpoint = format!(
        "https://api22-normal-c-useast2a.tiktokv.com/aweme/v1/feed/?aweme_id={id}&version_code=190500&app_name=trill&channel=googleplay&device_platform=android&aid=1180"
    );
    let mut request_headers = headers.to_vec();
    request_headers.push(("user-agent".into(), UA_APP.into()));
    request_headers.push(("x-request-type".into(), UA_APP_HEADERS_NAME.into()));
    let borrowed: Vec<(&str, &str)> = request_headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();

    let value = http.get_json("tiktok-feed", &endpoint, &borrowed)?;
    let item = list(&value, &["aweme_list"])
        .first()
        .ok_or_else(|| Error::Unsupported("the feed returned no item".into()))?;
    let media = from_item(item, id, page, headers);
    if media.formats.is_empty() && media.images.is_empty() {
        return Err(Error::Unsupported("the item has no media URL".into()));
    }
    Ok(media)
}

/// Builds a `Media` from an item, accepting both documented spellings: the web
/// page uses camelCase (`playAddr`), the app uses snake_case (`play_addr`).
fn from_item(item: &Value, id: &str, page: &str, headers: &[(String, String)]) -> Media {
    let mut media = Media::new(Site::TikTok, id.to_string(), page.to_string());

    media.title = text(item, &["desc"]).unwrap_or_default();
    media.uploader =
        text(item, &["author", "uniqueId"]).or_else(|| text(item, &["author", "unique_id"]));
    media.duration = uint(item, &["video", "duration"]);

    // The cover arrives in the same `{urlList: [...]}` shape as every other
    // asset, and `originCover` is the untouched frame where both are offered.
    let cover = url_list(item, &["video", "originCover"])
        .into_iter()
        .chain(url_list(item, &["video", "origin_cover"]))
        .chain(url_list(item, &["video", "cover"]))
        .next();
    if let Some(url) = cover {
        media.thumbnail = Some(Image {
            url,
            extension: "jpg",
            headers: headers.to_vec(),
        });
    }

    let mut candidates: Vec<(String, Option<u64>)> = Vec::new();
    for key in ["playAddr", "play_addr", "downloadAddr", "download_addr"] {
        if let Some(url) = text(item, &["video", key]) {
            candidates.push((url, None));
        }
        for url in url_list(item, &["video", key]) {
            candidates.push((url, None));
        }
    }
    for entry in list(item, &["video", "bitrateInfo"]) {
        let bitrate = uint(entry, &["Bitrate"]).or_else(|| uint(entry, &["bit_rate"]));
        for url in url_list(entry, &["PlayAddr"])
            .into_iter()
            .chain(url_list(entry, &["play_addr"]))
        {
            candidates.push((url, bitrate));
        }
    }

    let mut seen: Vec<String> = Vec::new();
    for (index, (url, bitrate)) in candidates.into_iter().enumerate() {
        if url.is_empty() || seen.contains(&url) {
            continue;
        }
        seen.push(url.clone());
        media.formats.push(Format {
            id: format!("tiktok-{}", index + 1),
            kind: StreamKind::Muxed,
            container: Container::Mp4,
            codec: String::new(),
            width: uint(item, &["video", "width"]).unwrap_or(0) as u32,
            height: uint(item, &["video", "height"]).unwrap_or(0) as u32,
            fps: 0,
            bitrate: bitrate.unwrap_or(0),
            bytes: uint(item, &["video", "dataSize"])
                .or_else(|| uint(item, &["video", "data_size"])),
            url: Some(url),
            headers: headers.to_vec(),
            source: "tiktok",
        });
    }

    for entry in list(item, &["imagePost", "images"]) {
        for url in url_list(entry, &["imageURL"]) {
            media.images.push(Image {
                url,
                extension: "jpg",
                headers: headers.to_vec(),
            });
        }
    }

    media
}

/// Reads `{"<key>": {"urlList": [...]}}` or `{"<key>": {"url_list": [...]}}`.
fn url_list(value: &Value, path: &[&str]) -> Vec<String> {
    let mut urls = Vec::new();
    // The web payload says `urlList`, the app payload says `UrlList`, and older
    // responses say `url_list`. All three carry the same array of strings.
    for key in ["urlList", "UrlList", "url_list"] {
        let mut joined = path.to_vec();
        joined.push(key);
        urls.extend(
            list(value, &joined)
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string)),
        );
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(url: &str) -> Option<String> {
        aweme_id(&Url::parse(url).unwrap())
    }

    #[test]
    fn finds_the_id() {
        assert_eq!(
            id("https://www.tiktok.com/@scout2015/video/6718335390845095173").as_deref(),
            Some("6718335390845095173")
        );
        assert_eq!(
            id("https://www.tiktok.com/@scout2015/photo/6718335390845095173").as_deref(),
            Some("6718335390845095173")
        );
        assert_eq!(
            id("https://www.tiktok.com/embed/v2/6718335390845095173").as_deref(),
            Some("6718335390845095173")
        );
        assert_eq!(id("https://www.tiktok.com/@scout2015"), None);
    }

    #[test]
    fn builds_formats_from_the_web_shape() {
        let item = serde_json::json!({
            "desc": "a clip",
            "author": {"uniqueId": "scout2015"},
            "video": {
                "duration": 15,
                "playAddr": "https://cdn.example/play.mp4",
                "downloadAddr": "https://cdn.example/download.mp4",
                "width": 1080,
                "height": 1920,
                "bitrateInfo": [
                    {"Bitrate": 2000000, "PlayAddr": {"UrlList": ["https://cdn.example/hd.mp4"]}}
                ]
            }
        });
        let media = from_item(&item, "1", "https://www.tiktok.com/@a/video/1", &[]);
        assert_eq!(media.title, "a clip");
        assert_eq!(media.uploader.as_deref(), Some("scout2015"));
        assert_eq!(media.duration, Some(15));
        assert_eq!(media.formats.len(), 3);
        assert!(media
            .formats
            .iter()
            .any(|f| f.url.as_deref() == Some("https://cdn.example/hd.mp4")
                && f.bitrate == 2_000_000));
        assert!(media.formats.iter().all(|f| f.kind == StreamKind::Muxed));
    }

    #[test]
    fn builds_formats_from_the_app_shape() {
        let item = serde_json::json!({
            "desc": "app shape",
            "author": {"unique_id": "someone"},
            "video": {
                "duration": 9,
                "play_addr": {"url_list": ["https://cdn.example/app.mp4"]},
                "data_size": 1234
            }
        });
        let media = from_item(&item, "2", "https://www.tiktok.com/@a/video/2", &[]);
        assert_eq!(media.uploader.as_deref(), Some("someone"));
        assert_eq!(media.formats.len(), 1);
        assert_eq!(media.formats[0].bytes, Some(1234));
    }

    #[test]
    fn an_origin_cover_preferred_over_a_cover() {
        let item = serde_json::json!({
            "desc": "a clip",
            "video": {
                "play_addr": {"url_list": ["https://cdn.example/v.mp4"]},
                "cover": {"url_list": ["https://cdn.example/cover.jpg"]},
                "origin_cover": {"url_list": ["https://cdn.example/origin.jpg"]}
            }
        });
        let media = from_item(&item, "4", "https://www.tiktok.com/@a/video/4", &[]);
        assert_eq!(
            media.thumbnail.map(|image| image.url),
            Some("https://cdn.example/origin.jpg".to_string())
        );
    }

    #[test]
    fn collects_image_posts() {
        let item = serde_json::json!({
            "desc": "photos",
            "imagePost": {
                "images": [
                    {"imageURL": {"urlList": ["https://cdn.example/1.jpg", "https://cdn.example/2.jpg"]}}
                ]
            }
        });
        let media = from_item(&item, "3", "https://www.tiktok.com/@a/photo/3", &[]);
        assert!(media.formats.is_empty());
        assert_eq!(media.images.len(), 2);
    }
}
