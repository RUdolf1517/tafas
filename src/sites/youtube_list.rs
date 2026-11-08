//! YouTube playlists and channels.
//!
//! Neither page carries a player response, so neither can be read by the video
//! extractor: what a list page has is *entries*. They come from InnerTube, the
//! same endpoint family `youtube.rs` already speaks to, with the browse
//! continuation token the page itself uses:
//!
//! | Page | Endpoint body |
//! | --- | --- |
//! | `/playlist?list=<id>` | `browse` with `browseId: VL<id>` |
//! | `/@handle`, `/channel/<id>` | `browse` with `browseId: <channel id>`, then its `videos` tab |
//!
//! Two things are deliberately not done here. A **lockup** (the newer
//! `lockupViewModel` shape) is not read yet: the pages this reads serve
//! `playlistVideoRenderer` and the `videoRenderer` a `richItemRenderer` wraps,
//! and a shape this build cannot
//! read is a refusal that names it rather than an empty list. And **paging is
//! bounded at one continuation per page**, so a channel with ten thousand videos
//! is refused with the number it did read rather than followed forever.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::http::{Http, UA_DESKTOP};
use crate::model::{Entry, Listing, Site};
use crate::sites::{at, json_after, text};
use crate::url::Url;

/// The same consent cookies the watch page uses; a European request without
/// them is answered with the interstitial, which lists nothing.
const HEADERS: [(&str, &str); 3] = [
    ("accept-language", "en-US,en;q=0.9"),
    ("cookie", "CONSENT=YES+cb; SOCS=CAI"),
    ("accept", "text/html,application/xhtml+xml"),
];

/// How many `browse` calls one list may take. A list longer than this is a refusal
/// naming both numbers, not a download that silently stops.
const MAX_PAGES: usize = 2;

/// What the page is: a playlist or a channel.
pub enum Kind {
    Playlist(String),
    Channel { handle: String, id: String },
}

/// Recognises the list pages of `youtube.com`.
///
/// `None` means "not a list page", and the caller falls through to the video
/// extractor — a `/watch?v=…&list=…` URL is a video with a playlist around it,
/// and the video is what the user named.
pub fn kind(url: &Url) -> Option<Kind> {
    let host = url.host()?;
    if !matches!(
        host,
        "youtube.com" | "music.youtube.com" | "youtube-nocookie.com"
    ) {
        return None;
    }
    if let Some(list) = url.query_param("list") {
        // `RD…` is the radio/mix a watch page generates; it is not a page.
        if !list.is_empty() && !list.starts_with("RD") && url.query_param("v").is_none() {
            return Some(Kind::Playlist(list));
        }
    }
    let segments = url.segments();
    match segments.first().copied() {
        Some(handle) if handle.starts_with('@') => Some(Kind::Channel {
            handle: handle.to_string(),
            id: String::new(),
        }),
        Some("channel") => {
            segments
                .get(1)
                .filter(|id| id.starts_with("UC"))
                .map(|id| Kind::Channel {
                    handle: String::new(),
                    id: (*id).to_string(),
                })
        }
        Some("c") | Some("user") => segments.get(1).map(|name| Kind::Channel {
            handle: (*name).to_string(),
            id: String::new(),
        }),
        _ => None,
    }
}

/// Reads a playlist or channel into a [`Listing`].
pub fn extract(http: &Http, url: &Url, kind: &Kind, raw: &str) -> Result<Listing> {
    match kind {
        Kind::Playlist(list) => playlist(http, list, raw),
        Kind::Channel { handle, id } => channel(http, url, handle, id, raw),
    }
}

fn playlist(http: &Http, list: &str, raw: &str) -> Result<Listing> {
    let key = api_key(http);
    let body = json!({
        "context": client_context(),
        "browseId": format!("VL{list}"),
    });
    let document = browse(http, &key, &body, raw)?;
    let mut listing = Listing::new(
        Site::YouTube,
        list.to_string(),
        format!("https://www.youtube.com/playlist?list={list}"),
    );
    listing.title = text(
        &document,
        &["header", "playlistHeaderRenderer", "title", "simpleText"],
    )
    .or_else(|| {
        text(
            &document,
            &["header", "playlistHeaderRenderer", "title", "runs"],
        )
    })
    .unwrap_or_else(|| list.to_string());

    let mut entries = Vec::new();
    let mut token = collect(&document, &mut entries);
    let mut pages = 1;
    while let Some(next) = token.take() {
        if pages >= MAX_PAGES {
            return Err(Error::Refused(format!(
                "youtube: playlist {list}: more than {} entries; this build reads the first {} \
                 (pass a shorter list or use --playlist-items on a video URL)",
                entries.len(),
                entries.len()
            )));
        }
        let body = json!({
            "context": client_context(),
            "continuation": next,
        });
        let document = browse(http, &key, &body, raw)?;
        token = collect(&document, &mut entries);
        pages += 1;
    }
    finish(listing, entries, "playlist", list)
}

fn channel(http: &Http, url: &Url, handle: &str, id: &str, raw: &str) -> Result<Listing> {
    let key = api_key(http);
    let channel_id = if !id.is_empty() {
        id.to_string()
    } else {
        resolve_handle(http, url, handle, raw)?
    };
    let body = json!({
        "context": client_context(),
        "browseId": channel_id,
    });
    let document = browse(http, &key, &body, raw)?;
    // The first browse answers with the channel's tabs; the videos tab is a
    // second browse with its own id, and reading the wrong one is how a channel
    // looks empty.
    let tabs = at(
        &document,
        &["contents", "twoColumnBrowseResultsRenderer", "tabs"],
    )
    .and_then(|tabs| tabs.as_array())
    .cloned()
    .unwrap_or_default();
    let videos_id = tabs
        .iter()
        .find(|tab| at(tab, &["tabRenderer", "title"]).and_then(|t| t.as_str()) == Some("Videos"))
        .and_then(|tab| {
            at(
                tab,
                &["tabRenderer", "endpoint", "browseEndpoint", "browseId"],
            )
        })
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| channel_id.clone());

    let mut listing = Listing::new(
        Site::YouTube,
        if handle.is_empty() { id } else { handle }.to_string(),
        url.to_string(),
    );
    listing.title = text(&document, &["metadata", "channelMetadataRenderer", "title"])
        .unwrap_or_else(|| listing.id.clone());

    // The first tab's response is already in hand when the channel id was the
    // videos tab; otherwise it has to be asked for.
    let mut entries = Vec::new();
    let mut token = if videos_id == channel_id {
        collect(&document, &mut entries)
    } else {
        let body = json!({ "context": client_context(), "browseId": videos_id });
        let videos = browse(http, &key, &body, raw)?;
        collect(&videos, &mut entries)
    };

    let mut pages = 1;
    while let Some(next) = token.take() {
        if pages >= MAX_PAGES {
            return Err(Error::Refused(format!(
                "youtube: channel {handle}: more than {} videos; this build reads the first {}, \
                 then stops rather than walking the whole channel",
                entries.len(),
                entries.len()
            )));
        }
        let body = json!({ "context": client_context(), "continuation": next });
        let document = browse(http, &key, &body, raw)?;
        token = collect(&document, &mut entries);
        pages += 1;
    }
    finish(listing, entries, "channel", id)
}

/// `/@handle` has no browse id in the URL, so the page is read for the one it
/// carries — `channelId` is in the first inline state object.
fn resolve_handle(http: &Http, url: &Url, handle: &str, raw: &str) -> Result<String> {
    let page = http
        .get_text(&url.to_string(), &HEADERS)
        .map_err(|error| Error::Refused(format!("youtube: {raw}: {error}")))?;
    for marker in ["ytInitialData = ", "\"channelId\":\"", "\"externalId\":\""] {
        if marker.starts_with("ytInitialData") {
            if let Some(document) = json_after(&page, marker) {
                if let Ok(value) = serde_json::from_str::<Value>(document) {
                    if let Some(id) = at(
                        &value,
                        &["metadata", "channelMetadataRenderer", "externalId"],
                    )
                    .and_then(|v| v.as_str())
                    {
                        return Ok(id.to_string());
                    }
                }
            }
            continue;
        }
        if let Some(index) = page.find(marker) {
            let tail = &page[index + marker.len()..];
            let id: String = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if id.starts_with("UC") && id.len() == 24 {
                return Ok(id);
            }
        }
    }
    Err(Error::Refused(format!(
        "youtube: {handle}: the channel page named no channel id; a handle that no longer exists \
         answers the same way, so check the URL in a browser"
    )))
}

/// The API key and the client context, from the page when it can be read and
/// from the documented web client when it cannot.
fn api_key(http: &Http) -> String {
    // Any YouTube page carries the key in its inline configuration; the watch
    // page is not needed and is not asked for.
    let page = http
        .get_text("https://www.youtube.com/playlist?list=WL&hl=en", &HEADERS)
        .unwrap_or_default();
    super::youtube::inline_api_key(&page)
        .unwrap_or_else(|| super::youtube::PUBLIC_WEB_API_KEY.to_string())
}

fn client_context() -> Value {
    json!({
        "client": {
            "clientName": "WEB",
            "clientVersion": "2.20241126.01.00",
            "hl": "en",
            "gl": "US",
            "userAgent": UA_DESKTOP
        }
    })
}

fn browse(http: &Http, key: &str, body: &Value, raw: &str) -> Result<Value> {
    let endpoint =
        format!("https://www.youtube.com/youtubei/v1/browse?key={key}&prettyPrint=false");
    let headers = [
        ("user-agent", UA_DESKTOP),
        ("origin", "https://www.youtube.com"),
        ("referer", "https://www.youtube.com/"),
    ];
    http.post_json("youtube-browse", &endpoint, &headers, body)
        .map_err(|error| match error {
            // A list page that answers 404 is a list that does not exist or is
            // private, and that is a refusal rather than a transport error.
            Error::Unsupported(_) => Error::Refused(format!(
                "youtube: {raw}: the page answers 404, so the list is gone or private"
            )),
            other => other,
        })
}

/// Walks one browse response for entries, and returns the continuation token it
/// ends with.
fn collect(document: &Value, entries: &mut Vec<Entry>) -> Option<String> {
    walk(document, entries);
    // `nextContinuationData` on the older shape, `continuationCommand` on the
    // newer one; both are one string, and a page with no token is the end.
    find_continuation(document)
}

/// Every entry shape the two page kinds use, found by key and then recursed
/// into. Only the *renderer* keys are named: a `richItemRenderer` wraps its
/// video in a `videoRenderer`, so matching both would count it twice.
fn walk(value: &Value, entries: &mut Vec<Entry>) {
    match value {
        Value::Object(map) => {
            if let Some(renderer) = map.get("playlistVideoRenderer") {
                if let Some(entry) = entry_from(renderer) {
                    entries.push(entry);
                }
            }
            if let Some(renderer) = map.get("videoRenderer") {
                if let Some(entry) = entry_from(renderer) {
                    entries.push(entry);
                }
            }
            if let Some(renderer) = map.get("gridVideoRenderer") {
                if let Some(entry) = entry_from(renderer) {
                    entries.push(entry);
                }
            }
            for nested in map.values() {
                walk(nested, entries);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, entries);
            }
        }
        _ => {}
    }
}

fn entry_from(renderer: &Value) -> Option<Entry> {
    let id = text(renderer, &["videoId"])?;
    if id.len() != 11 {
        return None;
    }
    let title = text(renderer, &["title", "simpleText"])
        .or_else(|| text(renderer, &["title", "runs"]))
        .or_else(|| {
            at(renderer, &["title", "runs"])
                .and_then(|runs| runs.as_array())
                .map(|runs| {
                    runs.iter()
                        .filter_map(|run| run.get("text").and_then(|t| t.as_str()))
                        .collect::<String>()
                })
        })
        .unwrap_or_else(|| id.clone());
    let duration = text(renderer, &["lengthText", "simpleText"])
        .as_deref()
        .and_then(seconds);
    Some(Entry {
        url: format!("https://www.youtube.com/watch?v={id}"),
        id,
        title,
        duration,
    })
}

/// `1:02:03` or `12:30` as seconds.
fn seconds(text: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut parts = 0;
    for part in text.trim().split(':') {
        total = total * 60 + part.trim().parse::<u64>().ok()?;
        parts += 1;
    }
    (parts > 0).then_some(total)
}

fn find_continuation(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(command) = map.get("continuationCommand") {
                if let Some(token) = command.get("token").and_then(|t| t.as_str()) {
                    return Some(token.to_string());
                }
            }
            if let Some(next) = map.get("nextContinuationData") {
                if let Some(token) = next.get("continuation").and_then(|t| t.as_str()) {
                    return Some(token.to_string());
                }
            }
            map.values().find_map(find_continuation)
        }
        Value::Array(items) => items.iter().find_map(find_continuation),
        _ => None,
    }
}

/// The last shared step: an empty list is a refusal, and the entries keep the
/// order the page gave them.
fn finish(mut listing: Listing, entries: Vec<Entry>, what: &str, id: &str) -> Result<Listing> {
    if entries.is_empty() {
        return Err(Error::Refused(format!(
            "youtube: {what} {id}: the page lists no videos; a private, empty or \
             members-only {what} answers the same way"
        )));
    }
    listing.entries = entries;
    Ok(listing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("a URL")
    }

    #[test]
    fn a_playlist_url_is_a_list_and_a_watch_url_with_a_list_is_not() {
        assert!(matches!(
            kind(&url("https://www.youtube.com/playlist?list=PL123")),
            Some(Kind::Playlist(list)) if list == "PL123"
        ));
        // A video inside a playlist is a video: the user named the video.
        assert!(
            kind(&url(
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL123"
            ))
            .is_none(),
            "a watch page with a list stays a video"
        );
        // A generated mix is not a page anybody can read later.
        assert!(kind(&url("https://www.youtube.com/watch?list=RDdQw4w9WgXcQ")).is_none());
        assert!(kind(&url("https://music.youtube.com/playlist?list=OLAK5uy_x")).is_some());
    }

    #[test]
    fn a_handle_and_a_channel_id_are_both_channels() {
        assert!(matches!(
            kind(&url("https://www.youtube.com/@NASA")),
            Some(Kind::Channel { handle, .. }) if handle == "@NASA"
        ));
        assert!(matches!(
            kind(&url("https://www.youtube.com/channel/UCabc")),
            Some(Kind::Channel { id, .. }) if id == "UCabc"
        ));
        // Not a list: a video page, and a host that is not YouTube at all.
        assert!(kind(&url("https://www.youtube.com/watch?v=dQw4w9WgXcQ")).is_none());
        assert!(kind(&url("https://vimeo.com/playlist?list=PL1")).is_none());
    }

    #[test]
    fn a_playlist_video_renderer_becomes_an_entry_with_its_duration() {
        let document = serde_json::json!({
            "contents": {
                "playlistVideoListRenderer": {
                    "contents": [
                        {"playlistVideoRenderer": {
                            "videoId": "dQw4w9WgXcQ",
                            "title": {"runs": [{"text": "A "}, {"text": "song"}]},
                            "lengthText": {"simpleText": "1:02:03"}
                        }},
                        {"playlistVideoRenderer": {
                            "videoId": "abcdefghijk",
                            "title": {"simpleText": "Second"}
                        }}
                    ]
                }
            }
        });
        let mut entries = Vec::new();
        walk(&document, &mut entries);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "dQw4w9WgXcQ");
        assert_eq!(entries[0].title, "A song", "runs are concatenated");
        assert_eq!(entries[0].duration, Some(3723));
        assert_eq!(
            entries[0].url,
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
        assert_eq!(entries[1].title, "Second");
    }

    #[test]
    fn a_rich_item_wrapped_video_is_read_and_a_locked_up_one_is_not() {
        let document = serde_json::json!({
            "contents": [
                {"richItemRenderer": {"content": {"videoRenderer": {
                    "videoId": "dQw4w9WgXcQ",
                    "title": {"runs": [{"text": "Wrapped"}]}
                }}}},
                // `lockupViewModel` is a shape this build does not read yet:
                // nothing is invented from it.
                {"lockupViewModel": {"contentId": "abcdefghijk"}}
            ]
        });
        let mut entries = Vec::new();
        walk(&document, &mut entries);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Wrapped");
    }

    #[test]
    fn a_continuation_token_is_found_under_either_shape() {
        let older = serde_json::json!({
            "continuationContents": {"continuation": {"nextContinuationData": {"continuation": "OLD"}}}
        });
        assert_eq!(find_continuation(&older).as_deref(), Some("OLD"));
        let newer = serde_json::json!({
            "contents": {"a": {"continuationItemRenderer": {"continuationEndpoint": {
                "continuationCommand": {"token": "NEW"}
            }}}}
        });
        assert_eq!(find_continuation(&newer).as_deref(), Some("NEW"));
        assert_eq!(find_continuation(&serde_json::json!({"x": 1})), None);
    }

    #[test]
    fn an_empty_list_is_a_named_refusal() {
        let listing = Listing::new(Site::YouTube, "PL1".into(), "https://x".into());
        let error = finish(listing, Vec::new(), "playlist", "PL1").expect_err("nothing listed");
        assert!(matches!(error, Error::Refused(_)));
        assert!(error.to_string().contains("private"), "{error}");
    }

    #[test]
    fn a_duration_text_is_seconds() {
        assert_eq!(seconds("0:30"), Some(30));
        assert_eq!(seconds("12:00"), Some(720));
        assert_eq!(seconds("1:02:03"), Some(3723));
        assert_eq!(seconds("live"), None);
    }

    #[test]
    fn an_id_that_is_not_eleven_characters_is_dropped() {
        let renderer = serde_json::json!({"videoId": "short", "title": {"simpleText": "x"}});
        assert!(entry_from(&renderer).is_none());
    }
}
