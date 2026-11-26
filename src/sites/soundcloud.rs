//! SoundCloud: sound only.
//!
//! The page carries `window.__sc_hydration = [...]`, the array the site's own
//! player renders from. One entry is the track (`"hydratable":"sound"`) and it
//! holds everything a capture needs except the bytes: the title, the uploader,
//! the duration, the artwork, and the transcodings the CDN will serve.
//!
//! The player's public client ID comes from the page or its JavaScript.
//! The transcoding endpoint returns a CDN URL for progressive MP3 or AAC HLS.
//! MP3 is copied verbatim and AAC is remuxed to M4A.
//!
//! A set (`/<artist>/sets/<slug>`) is a list of tracks and is read as one, in
//! [`soundcloud_set`](super::soundcloud_set): `--playlist-items`, one file per
//! entry, one dead track costing the others nothing. The list route runs first,
//! so a set URL never reaches this reader — and when it does (a build compiled
//! without the list route, a shape the list reader refuses), the refusal says
//! that the address is a set rather than calling it an unknown track.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::extractor_args::ExtractorArgs;
use crate::http::Http;
use crate::model::{Container, Format, Image, Media, Site, StreamKind};
use crate::sites::{at, list, text, uint};
use crate::url::Url;

const PAGE_HEADERS: [(&str, &str); 1] = [("accept", "text/html,application/xhtml+xml")];

pub fn extract(
    http: &Http,
    url: &Url,
    raw: &str,
    // This reader has no `--extractor-args` key; the parameter is uniform so
    // that the table of readers has one shape.
    _args: &ExtractorArgs,
) -> Result<Media> {
    // `on.soundcloud.com` is a redirector: the track's address is behind it.
    let url = if url.host() == Some("on.soundcloud.com") {
        Url::parse(&http.resolve(raw)?)?
    } else {
        url.clone()
    };
    let (artist, slug) = track_path(&url).ok_or_else(|| {
        // A set URL reaches here only when the list route did not run, and
        // “not a track URL” would hide the thing that has to happen instead.
        if is_set(&url) {
            Error::Usage(format!(
                "{raw} is a set: a list of tracks, which the list route reads and which this \
                 reader cannot turn into one file"
            ))
        } else {
            Error::Usage(format!(
                "{raw} is not a track URL: expected https://soundcloud.com/<artist>/<track>"
            ))
        }
    })?;
    let page_url = format!("https://soundcloud.com/{artist}/{slug}");
    let page = http.get_text(&page_url, &PAGE_HEADERS)?;

    let hydrated = hydration(&page).ok_or_else(|| {
        Error::Refused(format!(
            "soundcloud: {page_url}: the page carries no __sc_hydration document — a consent \
             wall, a block on this address, or a page that is not a track"
        ))
    })?;
    let (track, transcoding, container) = track_rendition(&hydrated)?;
    let client_id = player_client_id(http, &page).ok_or_else(|| {
        Error::Refused(format!(
            "soundcloud: {page_url}: could not find the player's client id"
        ))
    })?;

    let mut endpoint = reqwest::Url::parse(&transcoding)
        .ok()
        .filter(|url| {
            url.scheme() == "https"
                && matches!(
                    url.host_str(),
                    Some("api-v2.soundcloud.com" | "api.soundcloud.com")
                )
        })
        .ok_or_else(|| Error::Parse("soundcloud: invalid transcoding endpoint".into()))?;
    endpoint
        .query_pairs_mut()
        .append_pair("client_id", &client_id);
    let resolved = http.get_json("soundcloud-transcoding", endpoint.as_str(), &[])?;
    let Some(final_url) = text(&resolved, &["url"]) else {
        return Err(Error::Refused(format!(
            "soundcloud: {}: the transcoding answered no url; the track is not served to this \
             address, which is what a region block looks like",
            page_url
        )));
    };
    Ok(media_with_rendition(track, final_url, page_url, container))
}

/// The track and its progressive MP3 URL.
pub fn sound(value: &Value) -> Result<(&Value, String)> {
    let (track, url, container) = track_rendition(value)?;
    if container != Container::Mp3 {
        return Err(Error::Refused(
            "soundcloud: the track offers AAC HLS and no progressive MP3".into(),
        ));
    }
    Ok((track, url))
}

fn track_rendition(value: &Value) -> Result<(&Value, String, Container)> {
    let mut saw_playlist = false;
    for entry in value.as_array().map(|list| list.as_slice()).unwrap_or(&[]) {
        match text(entry, &["hydratable"]).as_deref() {
            Some("sound") => {
                let track = at(entry, &["data"]).ok_or_else(|| {
                    Error::Refused("soundcloud: the hydration entry carries no track".to_string())
                })?;
                let (url, container) = transcoding(track)?;
                return Ok((track, url, container));
            }
            Some("playlist") => saw_playlist = true,
            _ => {}
        }
    }
    if saw_playlist {
        // The list route reads a set; reaching this point means it did not run,
        // and “no track here” would send the reader looking for the wrong thing.
        return Err(Error::Unsupported(
            "soundcloud: this page is a set — a list of tracks — and the set's tracks are \
             captured one at a time; this build reads a set only through its list route"
                .to_string(),
        ));
    }
    Err(Error::Refused(
        "soundcloud: the hydration document lists no track".to_string(),
    ))
}

/// Prefer a progressive MP3 and fall back to AAC in an HLS playlist.
fn transcoding(track: &Value) -> Result<(String, Container)> {
    let mut aac = None;
    for entry in list(track, &["media", "transcodings"]) {
        if entry["snipped"].as_bool() == Some(true) {
            continue;
        }
        let Some(url) = text(entry, &["url"]) else {
            continue;
        };
        let protocol = text(entry, &["format", "protocol"]).unwrap_or_default();
        let mime = text(entry, &["format", "mime_type"]).unwrap_or_default();
        if protocol == "progressive" && mime == "audio/mpeg" {
            return Ok((url, Container::Mp3));
        }
        if protocol == "hls" && mime.starts_with("audio/mp4") && mime.contains("mp4a") {
            aac.get_or_insert(url);
        }
    }
    aac.map(|url| (url, Container::Hls)).ok_or_else(|| {
        Error::Refused(
            "soundcloud: the track offers no full-length MP3 or AAC HLS rendition".into(),
        )
    })
}

/// Builds media for a progressive MP3 stream.
pub fn parse(track: &Value, url: String, page_url: String) -> Media {
    media_with_rendition(track, url, page_url, Container::Mp3)
}

fn media_with_rendition(
    track: &Value,
    url: String,
    page_url: String,
    container: Container,
) -> Media {
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
    let codec = if container == Container::Hls {
        "aac"
    } else {
        "mp3"
    };
    media.formats.push(Format {
        id: format!("soundcloud-{codec}"),
        kind: StreamKind::Audio,
        container,
        codec: codec.into(),
        width: 0,
        height: 0,
        fps: 0,
        // The page names a preset, not a measured bitrate; claiming one would be
        // a number nobody checked.
        bitrate: 0,
        bytes: None,
        url: Some(url),
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
pub(crate) fn hydration(page: &str) -> Option<Value> {
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
pub(crate) fn client_id(page: &str) -> Option<String> {
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

/// The web player can publish its client ID in a JavaScript bundle.
pub(crate) fn player_client_id(http: &Http, page: &str) -> Option<String> {
    client_id(page).or_else(|| {
        const PREFIX: &str = r#" src="https://a-v2.sndcdn.com/assets/"#;
        page.rsplit(PREFIX).take(12).find_map(|tail| {
            let (file, _) = tail.split_once('"')?;
            if !file.ends_with(".js")
                || !file
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            {
                return None;
            }
            let script = http
                .get_text(&format!("https://a-v2.sndcdn.com/assets/{file}"), &[])
                .ok()?;
            client_id(&script)
        })
    })
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
    fn aac_hls_uses_the_full_rendition() {
        let value = json!({
            "media": { "transcodings": [
                { "url": "https://api-v2.soundcloud.com/preview", "snipped": true,
                  "format": { "protocol": "progressive", "mime_type": "audio/mpeg" } },
                { "url": "https://api-v2.soundcloud.com/full",
                  "format": { "protocol": "hls", "mime_type": "audio/mp4; codecs=\"mp4a.40.2\"" } }
            ] }
        });
        let (url, container) = transcoding(&value).unwrap();
        let media = media_with_rendition(
            &value,
            url,
            "https://soundcloud.com/artist/track".into(),
            container,
        );
        assert_eq!(media.formats[0].id, "soundcloud-aac");
        assert_eq!(media.formats[0].container, Container::Hls);
        assert_eq!(
            media.formats[0].url.as_deref(),
            Some("https://api-v2.soundcloud.com/full")
        );
    }

    #[test]
    fn a_track_without_mp3_or_aac_is_refused() {
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
        assert!(text.contains("no full-length MP3 or AAC"), "{text}");
        assert!(text.contains("HLS"), "{text}");
    }

    #[test]
    fn a_set_page_is_named_as_a_set_and_not_as_a_missing_track() {
        let value = json!([{ "hydratable": "playlist", "data": { "title": "a set" } }]);
        let error = sound(&value).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("set"), "{text}");
        assert!(
            text.contains("one at a time"),
            "the sentence says what to do with the tracks: {text}"
        );
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
