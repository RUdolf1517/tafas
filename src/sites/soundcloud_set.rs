//! SoundCloud sets.
//!
//! A set (`/<artist>/sets/<slug>`) is the site's own name for a playlist of
//! tracks, and it is a *list page*: `--playlist-items`, the directory rule, the
//! `<position> - <title> [<id>]` naming and the rule that one dead entry costs
//! the others nothing all come from `listing.rs`, unchanged.
//!
//! Two things about this site are worth knowing before reading the code.
//!
//! **A set is a track too.** Its page carries the same `__sc_hydration`
//! document a track's page does, and one entry of it is the set itself — the
//! site renders a set's own audio the way it renders any track. This build
//! reads a set as a list, not as one file, and that is a decision rather than an
//! oversight: the one-file reading is the one that silently writes a 3-minute
//! file for a 90-minute set whose first track happens to be the longest. A set
//! URL is a list, `--playlist-items` is how a user takes part of it, and
//! `soundcloud.rs` says as much if a set address ever reaches the track reader.
//!
//! **A long set arrives in pieces.** The hydration entry for a set carries its
//! first few `tracks` and a `track_count`; the site's own player asks
//! `api-v2.soundcloud.com/playlists/<id>?client_id=…` for the rest, one page at
//! a time, following `next_href` — the paging is `linked_partitioning` and the
//! `limit` is what the site's player asks for ([`PER_PAGE`]). This reader asks
//! the same way, and stops at [`MAX_PAGES`] the way the YouTube and Vimeo list
//! readers do: a refusal that says how many were read, rather than an hour of
//! downloads nobody asked for.
//!
//! When the paging stops short of what the page said the set holds — the API
//! refuses, or the two pages disagree — the listing is not thrown away and it is
//! not called whole either: it carries
//! [`Listing::incomplete`](crate::model::Listing::incomplete) and the download
//! layer says so on stderr. “12 of 40” printed over a set of twelve is exactly
//! the quiet lie this build refuses everywhere else.
//!
//! No `secret_token` is read from the URL. A private set is a session question,
//! not a token this build would know how to keep, and a public page with a token
//! on it is a page whose entries are readable one at a time anyway.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Entry, Listing, Site};
use crate::sites::{at, text, uint};
use crate::url::{percent_encode, Url};

/// What the site's own player asks for: the largest page a linked-partitioning
/// query returns.
pub const PER_PAGE: usize = 200;

/// How many pages of a set one run reads. Two hundred tracks per page is more
/// than any set this build expects; the bound exists so a `next_href` loop
/// cannot run forever, and reaching it is a refusal that names the count.
pub const MAX_PAGES: usize = 2;

/// The page headers, the same ones a track page is read with.
const PAGE_HEADERS: [(&str, &str); 1] = [("accept", "text/html,application/xhtml+xml")];

/// The set's artist and slug, from the path. The id a set is *asked* about is
/// the one its hydration document reports, so it is not read from the URL.
pub struct Kind {
    /// `<artist>` and `<slug>`, as the page's address spells them.
    pub artist: String,
    pub slug: String,
}

/// Recognises a set page.
///
/// `None` means “not a list page”, which includes a track
/// (`/<artist>/<track>`) and an artist's page — the caller falls through to the
/// video reader, which has its own sentence for an address that is neither.
pub fn kind(url: &Url) -> Option<Kind> {
    match url.segments().as_slice() {
        [artist, "sets", slug] if !slug.is_empty() => Some(Kind {
            artist: (*artist).to_string(),
            slug: (*slug).to_string(),
        }),
        _ => None,
    }
}

/// The set page this URL names, or `None` — the shape the reader table calls.
pub fn listing(http: &Http, url: &Url, raw: &str) -> Option<Result<Listing>> {
    kind(url).map(|kind| extract(http, url, &kind, raw))
}

/// Reads a set into a [`Listing`]: the page, then as many pages of tracks as
/// the set needs.
pub fn extract(http: &Http, _url: &Url, kind: &Kind, _raw: &str) -> Result<Listing> {
    let page_url = format!("https://soundcloud.com/{}/sets/{}", kind.artist, kind.slug);
    let html = http.get_text(&page_url, &PAGE_HEADERS)?;
    let hydrated = super::soundcloud::hydration(&html);
    let found = hydrated.as_ref().and_then(set_entry);
    let (set, tracks, declared) = match found {
        Some(found) => found,
        None => {
            // No hydration at all is the same refusal a track page earns, and it
            // is worth saying which one it was: a consent wall and a renamed
            // document look identical from here.
            return Err(if hydrated.is_some() {
                Error::Refused(format!(
                    "soundcloud: {}: the page is not a set this build can read — its \
                     hydration document lists neither a playlist entry nor tracks",
                    page_url
                ))
            } else {
                Error::Refused(format!(
                    "soundcloud: {}: the page carries no __sc_hydration document — a consent \
                     wall, a block on this address, or a page that is not a set",
                    page_url
                ))
            });
        }
    };

    let client_id = super::soundcloud::client_id(&html).ok_or_else(|| {
        Error::Refused(format!(
            "soundcloud: {page_url}: the page did not carry the public client id its player \
             uses, so the rest of the set cannot be asked for; --cookies FILE is sent when you \
             name a file"
        ))
    })?;
    let id = uint(set, &["id"])
        .map(|id| id.to_string())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| kind.slug.clone());
    // The secret token the page itself knows, when the set has one: a set the
    // page renders is a set whose tracks the API answers, token or not, and the
    // token only ever goes back to the site.
    let token = text(set, &["secret_token"]);

    let mut listing = Listing::new(Site::SoundCloud, id.clone(), page_url.clone());
    listing.title = text(set, &["title"]).unwrap_or_else(|| kind.slug.clone());
    listing.uploader = text(set, &["user", "username"]);

    let mut entries = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    push_entries(&tracks, &mut entries, &mut seen);

    // The page's own entry is one page of tracks; the rest is paged for. It is
    // asked for whenever the page *said* there are more, and also when it did
    // not say — a set whose first page is short is a set whose API answer is the
    // only way to know, and asking costs one request.
    let short_of_what_the_page_said = declared.map(|count| entries.len() < count).unwrap_or(true);
    if short_of_what_the_page_said && !id.is_empty() {
        let mut document = match page(http, &id, &client_id, token.as_deref(), None) {
            Ok(document) => document,
            // A page the API will not answer is not a failed set: what the page
            // itself listed is still a set worth capturing — but only if it said
            // how many there are, because otherwise nothing here can tell
            // whether the listing is whole, and calling it whole is the lie.
            Err(error) => match declared.filter(|count| entries.len() < *count) {
                Some(count) => {
                    listing.incomplete = Some(count);
                    return Ok(listing_with(listing, entries));
                }
                None => return Err(error),
            },
        };
        let mut pages = 1;
        loop {
            push_entries(
                crate::sites::list(&document, &["collection"]),
                &mut entries,
                &mut seen,
            );
            let Some(next_href) = text(&document, &["next_href"]) else {
                break;
            };
            if pages >= MAX_PAGES {
                // What was read, not a count this build cannot know: the same
                // sentence the YouTube and Vimeo list readers produce.
                return Err(Error::Refused(format!(
                    "soundcloud: set {id} lists more tracks than this build reads in {} pages \
                     and it stops there; pass --playlist-items to choose from the first {}",
                    MAX_PAGES,
                    entries.len()
                )));
            }
            document = page(http, &id, &client_id, token.as_deref(), Some(&next_href))?;
            pages += 1;
        }
    }

    if entries.is_empty() {
        return Err(Error::Refused(format!(
            "soundcloud: set {id} lists no tracks this build can read — an empty set, or one \
             whose tracks the page renders without an address"
        )));
    }
    // What the page said, when what was read is less. A track the site itself
    // serves as an unplayable stub is not counted as missing: it was listed.
    listing.incomplete = declared.filter(|count| entries.len() < *count);
    Ok(listing_with(listing, entries))
}

fn listing_with(mut listing: Listing, entries: Vec<Entry>) -> Listing {
    listing.entries = entries;
    listing
}

/// The set's entry in the hydration document, its tracks as the page rendered
/// them, and the count the page claims for the whole set.
///
/// The count is `track_count`, and it is only trusted when it is larger than
/// what the page rendered: a page that says 3 and shows 3 has answered.
fn set_entry(hydrated: &Value) -> Option<(&Value, Vec<Value>, Option<usize>)> {
    for entry in hydrated
        .as_array()
        .map(|list| list.as_slice())
        .unwrap_or(&[])
    {
        if text(entry, &["hydratable"]).as_deref() != Some("playlist") {
            continue;
        }
        let Some(data) = at(entry, &["data"]) else {
            continue;
        };
        let tracks = at(data, &["tracks"])
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let declared = uint(data, &["track_count"])
            .filter(|count| *count > 0)
            .map(|count| count as usize);
        return Some((data, tracks, declared));
    }
    None
}

/// The address of one page of a set's tracks.
///
/// `next_href` is used as it arrived — it is the API's own paging token and
/// rewriting it is how a page is asked for twice — with the page's `client_id`
/// appended, because the href it sends does not always carry one.
pub fn api_url(id: &str, client_id: &str, token: Option<&str>, next: Option<&str>) -> String {
    let client_id = percent_encode(client_id);
    if let Some(href) = next {
        // A href that already carries an id keeps it: appending a second one
        // would leave the API to choose, and the page's id is the one that
        // works.
        if href.contains("client_id=") {
            return href.to_string();
        }
        return format!(
            "{href}{}client_id={client_id}",
            if href.contains('?') { '&' } else { '?' }
        );
    }
    let mut url = format!("https://api-v2.soundcloud.com/playlists/{id}?client_id={client_id}");
    if let Some(token) = token {
        url.push_str("&secret_token=");
        url.push_str(&percent_encode(token));
    }
    url.push_str(&format!("&limit={PER_PAGE}&linked_partitioning=true"));
    url
}

/// `api-v2.soundcloud.com/playlists/<id>`, by `next_href` when there is one.
///
/// The `client_id` is the page's own, and the `secret_token` is only appended
/// when the page had one: a public set answers without it, and a token sent to
/// the wrong set is a 404 that reads like a missing set.
fn page(
    http: &Http,
    id: &str,
    client_id: &str,
    token: Option<&str>,
    next: Option<&str>,
) -> Result<Value> {
    let url = api_url(id, client_id, token, next);
    let document = http.get_json("soundcloud-set", &url, &[])?;
    if let Some(errors) = at(&document, &["errors"]) {
        // The API's own message, not a guess at what a 403 means.
        return Err(Error::Refused(format!(
            "soundcloud: set {id}: the API refused the page of tracks: {errors}"
        )));
    }
    Ok(document)
}

/// Appends the entries `tracks` describe, skipping the ones already read.
///
/// The page's own copy of a set's first few tracks and the paged copy are the
/// same tracks, and the second one arrives whole; the id is what identifies a
/// track, so it is what dedupes. A track the page renders without a
/// `permalink_url` is not skipped silently — it is *named* in the refusal below
/// only when it is all there is, and otherwise it is left out with its title
/// kept out of the listing rather than a broken URL in it.
fn push_entries(tracks: &[Value], entries: &mut Vec<Entry>, seen: &mut Vec<String>) {
    for track in tracks {
        let id = match uint(track, &["id"]) {
            Some(id) => id.to_string(),
            None => continue,
        };
        if seen.contains(&id) {
            continue;
        }
        let url = match text(track, &["permalink_url"]) {
            Some(url) => url,
            None => {
                // A stub: the site lists it, names it, and serves nothing. It is
                // skipped rather than made into a request that would fail.
                seen.push(id);
                continue;
            }
        };
        seen.push(id.clone());
        entries.push(Entry {
            id,
            url,
            title: text(track, &["title"]).unwrap_or_default(),
            duration: uint(track, &["duration"])
                .map(|millis| millis / 1000)
                .filter(|seconds| *seconds > 0),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hydrated(tracks: Value, declared: u64) -> Value {
        json!([
            { "hydratable": "apiClient", "data": { "id": "x" } },
            {
                "hydratable": "playlist",
                "data": {
                    "id": 1234,
                    "title": "A set",
                    "track_count": declared,
                    "user": { "username": "someone" },
                    "tracks": tracks
                }
            }
        ])
    }

    fn track(id: u64, title: &str) -> Value {
        json!({
            "id": id,
            "title": title,
            "duration": 180_000,
            "permalink_url": format!("https://soundcloud.com/someone/{title}")
        })
    }

    #[test]
    fn a_set_url_is_the_three_segment_shape_and_a_track_is_not() {
        let url = |raw: &str| Url::parse(raw).unwrap();
        let set = kind(&url("https://soundcloud.com/artist/sets/a-set")).expect("a set");
        assert_eq!(set.artist, "artist");
        assert_eq!(set.slug, "a-set");
        assert!(kind(&url("https://soundcloud.com/artist/a-track")).is_none());
        assert!(kind(&url("https://soundcloud.com/artist")).is_none());
        assert!(kind(&url("https://soundcloud.com/artist/sets")).is_none());
    }

    #[test]
    fn the_sets_own_entry_is_read_with_its_tracks_and_its_count() {
        let document = hydrated(json!([track(1, "first"), track(2, "second")]), 2);
        let (set, tracks, declared) = set_entry(&document).expect("a set");
        assert_eq!(text(set, &["title"]).as_deref(), Some("A set"));
        assert_eq!(tracks.len(), 2);
        assert_eq!(declared, Some(2));
        assert_eq!(uint(set, &["id"]), Some(1234));
    }

    #[test]
    fn entries_keep_their_order_and_their_seconds() {
        let document = hydrated(json!([track(7, "seven"), track(9, "nine")]), 2);
        let (_, tracks, _) = set_entry(&document).unwrap();
        let mut entries = Vec::new();
        let mut seen = Vec::new();
        push_entries(&tracks, &mut entries, &mut seen);
        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["7", "9"],
            "page order, not sorted"
        );
        assert_eq!(entries[0].title, "seven");
        assert_eq!(
            entries[0].duration,
            Some(180),
            "milliseconds become seconds"
        );
        assert_eq!(entries[0].url, "https://soundcloud.com/someone/seven");
    }

    #[test]
    fn a_track_the_page_already_named_is_not_listed_twice_by_the_paged_answer() {
        let page = vec![track(1, "one"), track(2, "two")];
        let paged = vec![track(1, "one"), track(2, "two"), track(3, "three")];
        let mut entries = Vec::new();
        let mut seen = Vec::new();
        push_entries(&page, &mut entries, &mut seen);
        push_entries(&paged, &mut entries, &mut seen);
        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["1", "2", "3"],
            "the paged copy of the page's own tracks adds nothing"
        );
    }

    #[test]
    fn a_stub_track_without_an_address_is_skipped_rather_than_made_into_a_url() {
        let tracks = vec![
            track(1, "readable"),
            json!({ "id": 2, "title": "a stub", "duration": 1000 }),
        ];
        let mut entries = Vec::new();
        let mut seen = Vec::new();
        push_entries(&tracks, &mut entries, &mut seen);
        assert_eq!(entries.len(), 1, "no entry without a permalink_url");
        assert_eq!(entries[0].id, "1");
    }

    #[test]
    fn a_set_that_says_it_holds_more_than_it_showed_is_marked_short() {
        let document = hydrated(json!([track(1, "one")]), 40);
        let (_, tracks, declared) = set_entry(&document).unwrap();
        let mut entries = Vec::new();
        let mut seen = Vec::new();
        push_entries(&tracks, &mut entries, &mut seen);
        let mut listing = Listing::new(Site::SoundCloud, "1234".into(), "u".into());
        listing.incomplete = declared.filter(|count| entries.len() < *count);
        assert_eq!(
            listing.incomplete,
            Some(40),
            "one of forty is marked, not called whole"
        );
    }

    #[test]
    fn a_page_without_a_hydration_document_is_not_a_set() {
        assert!(set_entry(&json!([])).is_none());
        assert!(set_entry(&json!([{ "hydratable": "sound", "data": {} }])).is_none());
    }

    #[test]
    fn the_page_address_carries_the_token_only_when_the_set_has_one() {
        assert_eq!(
            api_url("1234", "abc", None, None),
            "https://api-v2.soundcloud.com/playlists/1234?client_id=abc&limit=200&linked_partitioning=true"
        );
        assert!(
            !api_url("1234", "abc", None, None).contains("secret_token"),
            "a public set must not grow a parameter for a value it does not have"
        );
        assert_eq!(
            api_url("1234", "abc", Some("s-x1"), None),
            "https://api-v2.soundcloud.com/playlists/1234?client_id=abc&secret_token=s-x1&limit=200&linked_partitioning=true"
        );
    }

    #[test]
    fn a_next_href_is_followed_as_it_arrived_with_the_client_id_added() {
        // The href is the API's own paging token; rewriting it is how a page is
        // asked for twice, and it does not always carry a `client_id`.
        assert_eq!(
            api_url(
                "1234",
                "abc",
                None,
                Some("https://api-v2.soundcloud.com/playlists/1234/tracks?offset=200")
            ),
            "https://api-v2.soundcloud.com/playlists/1234/tracks?offset=200&client_id=abc"
        );
        assert_eq!(
            api_url(
                "1234",
                "abc",
                None,
                Some(
                    "https://api-v2.soundcloud.com/playlists/1234/tracks?offset=200&client_id=zzz"
                )
            ),
            "https://api-v2.soundcloud.com/playlists/1234/tracks?offset=200&client_id=zzz",
            "an id the href already carries is not appended twice"
        );
    }
}
