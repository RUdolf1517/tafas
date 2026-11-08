//! SoundCloud: sound only.
//!
//! The page carries `window.__sc_hydration = [...]`, the array the site's own
//! player renders from. One entry is the track (`"hydratable":"sound"`) and it
//! holds everything a capture needs except the bytes: the title, the uploader,
//! the duration, the artwork, and the transcodings the CDN will serve.
//!
//! A transcoding URL is not the media. It answers a small JSON document that
//! names the real CDN URL, and it only answers when the page's own public
//! `client_id` is appended — the same id the player sends, scraped from the page
//! rather than guessed at. So a capture is three requests: the page, the
//! transcoding, the bytes.
//!
//! Only the progressive `audio/mpeg` transcodings are taken: an MP3 is written
//! verbatim, with no container rebuilt around it, which is the one thing this
//! build can promise about it. A track that offers only the HLS/opus renditions
//! is refused by name — demuxing opus it cannot do, and pretending an MP3
//! exists when the page lists none would be a quiet lie.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Container, Format, Image, Media, Site, StreamKind};
use crate::sites::{at, list, text, uint};
use crate::url::Url;

const PAGE_HEADERS: [(&str, &str); 1] = [("accept", "text/html,application/xhtml+xml")];

pub fn extract(http: &Http, url: &Url, raw: &str) -> Result<Media> {
    // `on.soundcloud.com` is a redirector: the track's address is behind it.
    let url = if url.host() == Some("on.soundcloud.com") {
        Url::parse(&http.resolve(raw)?)?
    } else {
        url.clone()
    };
    if is_set(&url) {
        return Err(Error::Unsupported(format!(
            "{raw}: a set is a list of tracks, and this build captures one URL as one file; \
             the set's page lists the tracks that can be captured one at a time"
        )));
    }
    let (artist, slug) = track_path(&url).ok_or_else(|| {
        Error::Usage(format!(
            "{raw} is not a track URL: expected https://soundcloud.com/<artist>/<track>"
        ))
    })?;
    let page_url = format!("https://soundcloud.com/{artist}/{slug}");
    let page = http.get_text(&page_url, &PAGE_HEADERS)?;

    let hydrated = hydration(&page).ok_or_else(|| {
        Error::Refused(format!(
            "soundcloud: {page_url}: the page carries no __sc_hydration document — a consent \
             wall, a block on this address, or a page that is not a track"
        ))
    })?;
    let (track, transcoding) = sound(&hydrated)?;
    let client_id = client_id(&page).ok_or_else(|| {
        Error::Refused(format!(
            "soundcloud: {page_url}: the page did not carry the public client id its player uses, \
             so the transcoding cannot be asked for; --cookies FILE is sent when you name a file"
        ))
    })?;

    let mut media = parse(
        track,
        format!("{transcoding}?client_id={client_id}"),
        page_url,
    );

    // The transcoding answers a document naming the CDN URL, so this is the
    // second request, and the third is the bytes themselves.
    let endpoint = media.formats[0].url.clone().unwrap_or_default();
    let resolved = http.get_json("soundcloud-transcoding", &endpoint, &[])?;
    let Some(final_url) = text(&resolved, &["url"]) else {
        return Err(Error::Refused(format!(
            "soundcloud: {}: the transcoding answered no url; the track is not served to this \
             address, which is what a region block looks like",
            media.id
        )));
    };
    media.formats[0].url = Some(final_url);
    Ok(media)
}

/// The track object and its progressive transcoding URL, out of the hydration
/// array.
pub fn sound(value: &Value) -> Result<(&Value, String)> {
    let mut saw_playlist = false;
    for entry in value.as_array().map(|list| list.as_slice()).unwrap_or(&[]) {
        match text(entry, &["hydratable"]).as_deref() {
            Some("sound") => {
                let track = at(entry, &["data"]).ok_or_else(|| {
                    Error::Refused("soundcloud: the hydration entry carries no track".to_string())
                })?;
                return Ok((track, transcoding(track)?));
            }
            Some("playlist") => saw_playlist = true,
            _ => {}
        }
    }
    if saw_playlist {
        return Err(Error::Unsupported(
            "soundcloud: this page is a set, and a capture is one URL as one file".to_string(),
        ));
    }
    Err(Error::Refused(
        "soundcloud: the hydration document lists no track".to_string(),
    ))
}

/// The first progressive `audio/mpeg` transcoding, by name when there is none.
fn transcoding(track: &Value) -> Result<String> {
    let mut saw_hls = false;
    for entry in list(track, &["media", "transcodings"]) {
        let protocol = text(entry, &["format", "protocol"]).unwrap_or_default();
        let mime = text(entry, &["format", "mime_type"]).unwrap_or_default();
        if protocol == "hls" {
            saw_hls = true;
            continue;
        }
        if protocol == "progressive" && mime.contains("mpeg") {
            if let Some(url) = text(entry, &["url"]) {
                return Ok(url);
            }
        }
    }
    if saw_hls {
        return Err(Error::Refused(
            "soundcloud: the track offers only HLS (opus) renditions, and this build demuxes \
             neither opus nor a fragmented audio playlist"
                .to_string(),
        ));
    }
    Err(Error::Refused(
        "soundcloud: the track offers no progressive MP3 — it is either a Go+ track or only a \
         preview of one"
            .to_string(),
    ))
}

/// The track object, read into a [`Media`]. `url` is the transcoding; `extract`
/// replaces it with the CDN address the transcoding names.
pub fn parse(track: &Value, transcoding_url: String, page_url: String) -> Media {
    let id = uint(track, &["id"])
        .map(|id| id.to_string())
        .unwrap_or_else(|| page_url.rsplit('/').next().unwrap_or("track").to_string());
    let mut media = Media::new(Site::SoundCloud, id, page_url);
    media.title = text(track, &["title"]).unwrap_or_default();
    media.uploader = text(track, &["user", "username"]);
    media.duration = uint(track, &["duration"])
        .map(|millis| millis / 1000)
        .filter(|seconds| *seconds > 0);
    media.thumbnail = text(track, &["artwork_url"])
        .or_else(|| text(track, &["user", "avatar_url"]))
        .map(|url| Image {
            url,
            extension: "jpg",
            headers: request_headers(),
        });
    media.formats.push(Format {
        id: "soundcloud-mp3".to_string(),
        kind: StreamKind::Audio,
        container: Container::Mp3,
        codec: "mp3".to_string(),
        width: 0,
        height: 0,
        fps: 0,
        // The page names a preset, not a measured bitrate; claiming one would be
        // a number nobody checked.
        bitrate: 0,
        bytes: None,
        url: Some(transcoding_url),
        headers: request_headers(),
        source: "soundcloud-hydration",
    });
    media
}

fn request_headers() -> Vec<(String, String)> {
    vec![(
        "user-agent".to_string(),
        crate::http::UA_DESKTOP.to_string(),
    )]
}

/// `window.__sc_hydration = [ ... ];` out of the page, as JSON.
fn hydration(page: &str) -> Option<Value> {
    let start = page.find("__sc_hydration")?;
    let tail = &page[start..];
    let open = tail.find('[')?;
    let bytes = tail.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    let text = tail.get(open..=index)?;
                    return serde_json::from_str(text).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// The public `client_id` the player itself sends. It appears in the page as
/// `client_id=<32 alphanumerics>`, inside a script URL and inside inline JSON,
/// so both spellings are accepted.
fn client_id(page: &str) -> Option<String> {
    let marker = "client_id";
    let mut rest = page;
    while let Some(index) = rest.find(marker) {
        rest = &rest[index + marker.len()..];
        let tail = rest.trim_start_matches(['=', '"', ':', '\\', ' ']);
        let id: String = tail
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric())
            .collect();
        if id.len() >= 16 {
            return Some(id);
        }
    }
    None
}

/// `<artist>/<track>` from the path. Everything else is not a track.
fn track_path(url: &Url) -> Option<(String, String)> {
    let segments = url.segments();
    let reserved = [
        "you", "discover", "search", "stream", "upload", "pages", "charts", "tags", "popular",
        "sets",
    ];
    match segments.as_slice() {
        [artist, slug] if !reserved.contains(artist) && !slug.is_empty() => {
            Some(((*artist).to_string(), (*slug).to_string()))
        }
        _ => None,
    }
}

fn is_set(url: &Url) -> bool {
    let segments = url.segments();
    matches!(segments.as_slice(), [_, "sets", _])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page() -> String {
        format!(
            "<script>window.__sc_hydration = {}</script>\
             <script src=\"https://a-v2.sndcdn.com/assets/app.js?client_id={}\"></script>",
            json!([
                { "hydratable": "apiClient", "data": { "id": "x" } },
                {
                    "hydratable": "sound",
                    "data": {
                        "id": 249492876,
                        "title": "A track with a } brace",
                        "duration": 245_000,
                        "artwork_url": "https://i1.sndcdn.com/artworks-large.jpg",
                        "user": { "username": "someone", "avatar_url": "https://i1.sndcdn.com/a-large.jpg" },
                        "media": { "transcodings": [
                            { "url": "https://api-v2.soundcloud.com/media/sound/1/stream/hls",
                              "preset": "mp3_0_1", "format": { "protocol": "hls", "mime_type": "audio/mpeg" } },
                            { "url": "https://api-v2.soundcloud.com/media/sound/1/stream/progressive",
                              "preset": "mp3_0_1", "format": { "protocol": "progressive", "mime_type": "audio/mpeg" } }
                        ] }
                    }
                }
            ]),
            "0123456789abcdef0123456789abcdef"
        )
    }

    #[test]
    fn the_hydration_document_is_found_and_parsed() {
        let value = hydration(&page()).expect("the document is there");
        assert_eq!(value.as_array().map(|list| list.len()), Some(2));
        let (track, transcoding) = sound(&value).expect("a track");
        assert_eq!(track["title"], "A track with a } brace");
        assert!(
            transcoding.ends_with("/progressive"),
            "the progressive transcoding is chosen: {transcoding}"
        );
    }

    #[test]
    fn the_public_client_id_is_read_from_both_spellings() {
        assert_eq!(
            client_id(&page()).as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            client_id(r#"{"client_id":"abcdefabcdefabcdefabcdef12345"}"#).as_deref(),
            Some("abcdefabcdefabcdefabcdef12345")
        );
        assert_eq!(client_id("nothing here"), None);
    }

    #[test]
    fn a_track_becomes_one_mp3_format() {
        let value = hydration(&page()).unwrap();
        let (track, transcoding) = sound(&value).unwrap();
        let media = parse(
            track,
            transcoding,
            "https://soundcloud.com/someone/a-track".to_string(),
        );
        assert_eq!(media.site, Site::SoundCloud);
        assert_eq!(media.title, "A track with a } brace");
        assert_eq!(media.uploader.as_deref(), Some("someone"));
        assert_eq!(media.duration, Some(245), "milliseconds become seconds");
        assert_eq!(media.id, "249492876");
        assert_eq!(media.formats.len(), 1);
        assert_eq!(media.formats[0].container, Container::Mp3);
        assert_eq!(media.formats[0].kind, StreamKind::Audio);
        assert_eq!(media.thumbnail.as_ref().unwrap().extension, "jpg");
    }

    #[test]
    fn a_track_without_a_progressive_transcoding_is_refused() {
        let value = json!([
            { "hydratable": "sound", "data": {
                "title": "opus only",
                "media": { "transcodings": [
                    { "url": "https://api-v2/x/hls", "format": { "protocol": "hls", "mime_type": "audio/mpeg" } }
                ] }
            } }
        ]);
        let error = sound(&value).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("opus"), "{text}");
        assert!(text.contains("HLS"), "{text}");
    }

    #[test]
    fn a_set_is_refused_by_name() {
        let value = json!([{ "hydratable": "playlist", "data": { "title": "a set" } }]);
        let error = sound(&value).unwrap_err();
        assert!(error.to_string().contains("set"), "{error}");
    }

    #[test]
    fn track_paths_and_sets() {
        let url = |raw: &str| Url::parse(raw).unwrap();
        assert_eq!(
            track_path(&url("https://soundcloud.com/artist/track-name")),
            Some(("artist".into(), "track-name".into()))
        );
        assert_eq!(track_path(&url("https://soundcloud.com/artist")), None);
        assert_eq!(track_path(&url("https://soundcloud.com/you/likes")), None);
        assert!(is_set(&url("https://soundcloud.com/artist/sets/a-set")));
        assert!(!is_set(&url("https://soundcloud.com/artist/track-name")));
    }
}
