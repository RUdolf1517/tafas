//! Vimeo albums and showcases.
//!
//! An album is not a video and has no player config of its own: it is a page
//! that names other videos. This build reads it the way Vimeo's own showcase
//! page does, in three steps:
//!
//! 1. `https://vimeo.com/_next/viewer` answers a short-lived **JWT** and an
//!    `xsrft` token. The JWT is what the unauthenticated web player uses to
//!    read a public album, and it is asked for rather than invented — there is
//!    no app secret in this binary.
//! 2. `https://api.vimeo.com/albums/<id>?fields=name,description,privacy`
//!    names the album. A password-protected album is refused *here*, before a
//!    single entry is listed, because the entry URLs it would produce cannot be
//!    read either.
//! 3. `https://api.vimeo.com/albums/<id>/videos?per_page=…&page=…` pages
//!    through the entries. Two pages, one hundred each, the same bound the
//!    YouTube list reader uses and for the same reason: a showcase with ten
//!    thousand videos is not a capture anybody starts by accident, and a
//!    refusal that says how many it read is better than an hour of downloads.
//!
//! A non-numeric showcase address (`/showcase/BethelTally-Homegoing-Services`)
//! is resolved to its id by `https://vimeo.com/showcase/<slug>/auth`, which is
//! the endpoint the page itself calls; the id it answers is the one the API
//! takes.
//!
//! An entry becomes a URL, not a stream: each one is read later by
//! `vimeo::extract`, on its own, which is what makes one dead video cost the
//! others nothing.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Entry, Listing, Site};
use crate::sites::{at, text};
use crate::url::Url;

const VIEWER: &str = "https://vimeo.com/_next/viewer";

/// Entries asked for per page. Vimeo's own showcase page uses 100.
const PER_PAGE: u32 = 100;

/// How many `videos` pages one album may take.
const MAX_PAGES: u32 = 2;

/// The pages this reader answers for. `staffpicks` and a bare user page are
/// deliberately not here: they are catalogs rather than collections, and
/// nothing in the roadmap asks for them.
pub enum Kind {
    /// `/album/<id>` and `/showcase/<id-or-slug>`.
    Album(String),
}

/// Recognises an album or a showcase page of `vimeo.com`.
///
/// `None` means "not a list page", and the caller falls through to the video
/// reader — `/album/2632481/video/123456789` is a video inside an album, and
/// the video is what the user named.
pub fn kind(url: &Url) -> Option<Kind> {
    if url.host() != Some("vimeo.com") {
        return None;
    }
    let segments = url.segments();
    let id = match segments.first().copied() {
        Some("album") | Some("showcase") => segments.get(1)?,
        _ => return None,
    };
    // A deeper address is a video inside the album, or the album's embed page.
    // Both name something narrower than the album, so neither is a list here.
    if segments.len() > 2 {
        return None;
    }
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    Some(Kind::Album(id.to_string()))
}

/// The album page this URL names, or `None` — the shape the reader table calls.
pub fn listing(http: &Http, url: &Url, raw: &str) -> Option<Result<Listing>> {
    kind(url).map(|kind| extract(http, url, &kind, raw))
}

/// Reads an album into a [`Listing`].
pub fn extract(http: &Http, url: &Url, kind: &Kind, raw: &str) -> Result<Listing> {
    let Kind::Album(id) = kind;
    let jwt = viewer_jwt(http, raw)?;
    let album_id = if looks_numeric(id) {
        id.clone()
    } else {
        resolve_slug(http, id, raw)?
    };
    let album = album_document(http, &album_id, &jwt, raw)?;
    if at(&album, &["privacy", "view"]).and_then(Value::as_str) == Some("password") {
        return Err(Error::Refused(format!(
            "vimeo: album {album_id}: it is protected by a password, and this build has no way \
             to enter one; the videos are readable one at a time if their own URLs are public"
        )));
    }

    let mut listing = Listing::new(
        Site::Vimeo,
        album_id.clone(),
        format!("https://vimeo.com/showcase/{album_id}"),
    );
    listing.title = text(&album, &["name"]).unwrap_or_else(|| album_id.clone());
    listing.uploader = text(&album, &["user", "name"]);

    let mut entries = Vec::new();
    let mut pages = 0;
    loop {
        if pages >= MAX_PAGES {
            return Err(Error::Refused(format!(
                "vimeo: album {album_id}: more than {} videos; this build reads the first {} \
                 and stops rather than walking the whole showcase",
                entries.len(),
                entries.len()
            )));
        }
        let before = entries.len();
        let page = videos_page(http, &album_id, &jwt, pages + 1, raw)?;
        pages += 1;
        collect(&page, &mut entries);
        if !another_page(&page, before, entries.len()) {
            break;
        }
    }
    if entries.is_empty() {
        return Err(Error::Refused(format!(
            "vimeo: album {album_id}: the album lists no videos; an empty album and one whose \
             videos were all deleted answer the same way"
        )));
    }
    // `url` is kept only so that a caller following a redirect knows where it
    // landed; the page the entries came from is the canonical showcase URL.
    debug_assert_eq!(url.host(), Some("vimeo.com"));
    listing.entries = entries;
    Ok(listing)
}

/// The `jwt` the web player sends. `xsrft` is asked for in the same request
/// only because Vimeo returns both; nothing here logs in.
fn viewer_jwt(http: &Http, raw: &str) -> Result<String> {
    let headers = [("accept", "application/json")];
    let viewer = http
        .get_json("vimeo-viewer", VIEWER, &headers)
        .map_err(|error| {
            Error::Refused(format!(
                "vimeo: {raw}: the album needs the web player's token, and \
                 vimeo.com/_next/viewer did not answer one: {error}"
            ))
        })?;
    text(&viewer, &["jwt"]).ok_or_else(|| {
        Error::Refused(format!(
            "vimeo: {raw}: vimeo.com/_next/viewer answered without a jwt, so the album cannot be \
             read without a session"
        ))
    })
}

/// `/showcase/<slug>` → the numeric album id, through the endpoint the page
/// itself calls with the slug.
fn resolve_slug(http: &Http, slug: &str, raw: &str) -> Result<String> {
    let endpoint = format!("https://vimeo.com/showcase/{}/auth", path_segment(slug));
    let headers = [
        ("accept", "application/json"),
        ("x-requested-with", "XMLHttpRequest"),
    ];
    let answer = http.get_json_opt("vimeo-album-auth", &endpoint, &headers)?;
    let Some(answer) = answer else {
        return Err(Error::Refused(format!(
            "vimeo: {raw}: the showcase address answers 404; a showcase that was deleted, made \
             private, or is named differently answers the same way"
        )));
    };
    at(&answer, &["metadata", "id"])
        .and_then(Value::as_u64)
        .map(|id| id.to_string())
        .ok_or_else(|| {
            Error::Refused(format!(
                "vimeo: {raw}: the showcase named no album id, so there is nothing to list"
            ))
        })
}

fn album_document(http: &Http, album_id: &str, jwt: &str, raw: &str) -> Result<Value> {
    let endpoint = format!(
        "https://api.vimeo.com/albums/{album_id}?fields=name,description,privacy,user.name"
    );
    let authorization = format!("jwt {jwt}");
    let headers = [
        ("accept", "application/json"),
        ("authorization", authorization.as_str()),
    ];
    match http.get_json_opt("vimeo-album", &endpoint, &headers)? {
        Some(album) => Ok(album),
        None => Err(Error::Refused(format!(
            "vimeo: {raw}: the album is gone, private, or belonging to an account this build \
             cannot read without --cookies FILE"
        ))),
    }
}

/// The album id and the page number name this request; the URL the user typed
/// would only be repeated.
fn videos_page(http: &Http, album_id: &str, jwt: &str, page: u32, _raw: &str) -> Result<Value> {
    let endpoint = format!(
        "https://api.vimeo.com/albums/{album_id}/videos?fields=link,uri,name,duration&per_page={PER_PAGE}&page={page}"
    );
    let authorization = format!("jwt {jwt}");
    let headers = [
        ("accept", "application/json"),
        ("authorization", authorization.as_str()),
    ];
    match http.get_json_opt("vimeo-album-videos", &endpoint, &headers)? {
        Some(document) => Ok(document),
        None => Err(Error::Refused(format!(
            "vimeo: album {album_id}: page {page} of the album answers 404; a private album does \
             the same, and --cookies FILE is sent when you name a file"
        ))),
    }
}

/// One page of the API's `data` array, as entries, in the order it arrived.
fn collect(document: &Value, entries: &mut Vec<Entry>) {
    let Some(items) = document.get("data").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        if let Some(entry) = entry_from(item) {
            entries.push(entry);
        }
    }
}

/// `{"uri": "/videos/76979871", "link": "https://vimeo.com/76979871", …}` as an
/// entry. The id comes from `uri` because `link` can be a custom address that
/// this build's own URL reader would then have to guess about.
fn entry_from(item: &Value) -> Option<Entry> {
    let uri = text(item, &["uri"])?;
    let id = uri.rsplit('/').next()?.trim().to_string();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let title = text(item, &["name"]).unwrap_or_else(|| id.clone());
    let duration = at(item, &["duration"])
        .and_then(Value::as_f64)
        .map(|seconds| seconds.round().max(0.0) as u64)
        .filter(|seconds| *seconds > 0);
    Some(Entry {
        url: format!("https://vimeo.com/{id}"),
        id,
        title,
        duration,
    })
}

/// Whether the API says there is another page after this one.
///
/// The paging block is the truth when it is there; a full page with no paging
/// block is treated as the last one, because asking for a page that does not
/// exist is an error rather than an empty list.
fn has_more(document: &Value, read: usize) -> bool {
    let paging = document.get("paging");
    let page = paging
        .and_then(|paging| paging.get("page"))
        .and_then(Value::as_u64);
    let per_page = paging
        .and_then(|paging| paging.get("per_page"))
        .and_then(Value::as_u64);
    let total = paging
        .and_then(|paging| paging.get("total"))
        .and_then(Value::as_u64);
    match (page, per_page, total) {
        (Some(page), Some(per_page), Some(total)) => page * per_page < total,
        _ => read >= PER_PAGE as usize,
    }
}

/// The collection has to say it has a next page *and* that page has to bring
/// something new, or paging would loop forever on an album whose `paging` block
/// never catches up with its entries.
fn another_page(document: &Value, read_before: usize, read_after: usize) -> bool {
    has_more(document, read_after) && read_after > read_before
}

/// A slug is put back into a path, so it may not carry one.
fn path_segment(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect()
}

fn looks_numeric(value: &str) -> bool {
    !value.is_empty() && value.len() <= 12 && value.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("a URL")
    }

    #[test]
    fn an_album_and_a_showcase_are_lists_and_a_video_inside_one_is_not() {
        assert!(matches!(
            kind(&url("https://vimeo.com/album/2632481")),
            Some(Kind::Album(id)) if id == "2632481"
        ));
        assert!(matches!(
            kind(&url("https://vimeo.com/showcase/BethelTally-Homegoing-Services")),
            Some(Kind::Album(id)) if id == "BethelTally-Homegoing-Services"
        ));
        assert!(
            kind(&url("https://vimeo.com/showcase/10677689/video/505682113")).is_none(),
            "a video inside a showcase is a video"
        );
        assert!(
            kind(&url("https://vimeo.com/showcase/10677689/embed")).is_none(),
            "the embed page names one player, not the album"
        );
        assert!(kind(&url("https://player.vimeo.com/video/76979871")).is_none());
        assert!(kind(&url("https://vimeo.com/76979871")).is_none());
    }

    #[test]
    fn a_page_of_the_api_becomes_entries_with_their_ids_and_durations() {
        let document = json!({
            "data": [
                { "uri": "/videos/76979871", "link": "https://vimeo.com/76979871",
                  "name": "The New Vimeo Player", "duration": 62.4 },
                { "uri": "/videos/144579403", "link": "https://vimeo.com/offthepicture/salesmanship",
                  "name": "SALESMANSHIP" },
                // An on-demand title is a `/ondemand/…` uri, not a video: it is
                // not something this build can read, and it is not invented.
                { "uri": "/ondemand/nazmaalik", "name": "Nazma" }
            ],
            "paging": { "page": 1, "per_page": 100, "total": 3 }
        });
        let mut entries = Vec::new();
        collect(&document, &mut entries);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "76979871");
        assert_eq!(
            entries[0].url, "https://vimeo.com/76979871",
            "the canonical video URL, not the album's custom one"
        );
        assert_eq!(entries[0].duration, Some(62));
        assert_eq!(entries[1].title, "SALESMANSHIP");
        assert_eq!(entries[1].duration, None);
    }

    #[test]
    fn paging_stops_when_the_api_says_so() {
        assert!(!has_more(
            &json!({ "paging": { "page": 2, "per_page": 100, "total": 150 } }),
            150
        ));
        assert!(has_more(
            &json!({ "paging": { "page": 1, "per_page": 100, "total": 150 } }),
            100
        ));
        assert!(
            has_more(&json!({ "data": [] }), 100),
            "without a paging block, a full page means there may be more"
        );
        assert!(!has_more(&json!({ "data": [] }), 3));
    }

    #[test]
    fn a_page_that_brings_nothing_new_ends_the_walk() {
        // A `paging` block that still claims a next page cannot make the walk
        // loop: an entry count that did not move is the end.
        let document = json!({ "paging": { "page": 1, "per_page": 100, "total": 500 } });
        assert!(!another_page(&document, 100, 100));
        assert!(another_page(&document, 100, 200));
    }

    #[test]
    fn a_slug_cannot_add_a_path_segment() {
        assert_eq!(path_segment("../etc/passwd"), "..etcpasswd");
        assert_eq!(
            path_segment("BethelTally-Homegoing"),
            "BethelTally-Homegoing"
        );
        assert!(looks_numeric("2632481"));
        assert!(!looks_numeric("BethelTally"));
        assert!(!looks_numeric(""));
    }

    #[test]
    fn the_album_takes_its_name_from_the_api() {
        let album =
            json!({ "name": "Staff Favorites: November 2013", "privacy": { "view": "anybody" } });
        assert_eq!(
            text(&album, &["name"]).as_deref(),
            Some("Staff Favorites: November 2013")
        );
        assert_ne!(
            at(&album, &["privacy", "view"]).and_then(Value::as_str),
            Some("password")
        );
    }
}
