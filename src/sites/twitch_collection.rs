//! Twitch collections.
//!
//! A collection is the site's own name for a hand-picked group of recordings a
//! channel made — a playthrough's parts, a season, an event. It is not a VOD
//! and has no playback token: it is a list, and each of its items is a VOD that
//! `twitch::extract` reads later on its own.
//!
//! One batched GraphQL request is enough, against the same endpoint the VOD
//! reader already speaks to: `CollectionSideBar` by persisted-query hash, with
//! `variables: { collectionID }` and the public web client id in a header. The
//! answer holds the title and `items.edges[].node`, and a node is a video: its
//! `id` becomes `https://www.twitch.tv/videos/<id>`.
//!
//! Collections are a feature Twitch has been winding down on the web — the tab
//! is gone from most channel pages — while the API operation still answers for
//! the collections that exist. This reader therefore trusts the answer and not
//! the site's navigation: a collection that no longer exists answers `null`,
//! which is a named refusal rather than an empty list.
//!
//! `/collections/<id>` is the only shape read. A channel's own collections tab
//! (`/<name>/videos?filter=collections`) lists *collections*, which would make
//! `--playlist-items` address a list of lists; that is refused by name, in the
//! same spirit as the nested-list refusal the download layer already has.

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::{Entry, Listing, Site};
use crate::sites::{at, text};
use crate::url::Url;

/// The persisted-query hash the site's player uses for `CollectionSideBar`.
/// Twitch asks by hash rather than by query text; if it ever changes, the API
/// answers an error object, which is a refusal that quotes it.
const COLLECTION_HASH: &str = "016e1e4ccee0eb4698eb3bf1a04dc1c077fb746c78c82bac9a8f0289658fbd1a";

pub enum Kind {
    /// `/collections/<id>`.
    Collection(String),
}

/// Recognises a collection page.
///
/// `None` means "not a list page", which includes a VOD (`/videos/<id>`) and a
/// channel — the caller falls through to the video reader, which refuses the
/// channel with its own sentence.
pub fn kind(url: &Url) -> Option<Kind> {
    if url.host() != Some("twitch.tv") {
        return None;
    }
    let segments = url.segments();
    if segments.first().copied() != Some("collections") {
        return None;
    }
    let id = segments.get(1)?.trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(Kind::Collection(id.to_string()))
}

/// The collection page this URL names, or `None` — the shape the reader table
/// calls.
pub fn listing(http: &Http, url: &Url, raw: &str) -> Option<Result<Listing>> {
    kind(url).map(|kind| extract(http, url, &kind, raw))
}

/// Reads a collection into a [`Listing`].
///
/// The raw URL is not quoted in a refusal here: the collection id is what names
/// the page, and a refusal that repeats the URL the user just typed says less
/// than the one that names the collection and what happened to it.
pub fn extract(http: &Http, url: &Url, kind: &Kind, _raw: &str) -> Result<Listing> {
    let Kind::Collection(id) = kind;
    let body = json!([{
        "operationName": "CollectionSideBar",
        "variables": { "collectionID": id },
        "extensions": { "persistedQuery": { "version": 1, "sha256Hash": COLLECTION_HASH } }
    }]);
    let headers = [("client-id", super::twitch::PUBLIC_WEB_CLIENT_ID)];
    let answer = http.post_json(
        "twitch-collection",
        "https://gql.twitch.tv/gql",
        &headers,
        &body,
    )?;
    let mut listing = Listing::new(
        Site::Twitch,
        id.clone(),
        format!("https://www.twitch.tv/collections/{id}"),
    );
    listing.title = parse(&answer, id, &mut listing.entries)?;
    // `url` is what the user typed; the listing points at the canonical page.
    debug_assert_eq!(url.host(), Some("twitch.tv"));
    Ok(listing)
}

/// Fills `entries` from the answer and returns the collection's title.
///
/// Split from the fetch so the shapes are tested against a fixture: a change on
/// the site's side has to produce a refusal in a bug report rather than a
/// silent empty list.
pub fn parse(answer: &Value, id: &str, entries: &mut Vec<Entry>) -> Result<String> {
    let batch = answer.as_array().ok_or_else(|| {
        Error::Refused(format!(
            "twitch: collection {id}: the API answered something that is not a batch of \
             operations"
        ))
    })?;
    if let Some(message) = batch.iter().find_map(first_error) {
        return Err(Error::Refused(format!(
            "twitch: collection {id}: the API refused the query: {message}"
        )));
    }
    let collection = batch
        .first()
        .and_then(|entry| at(entry, &["data", "collection"]))
        .filter(|collection| !collection.is_null());
    let Some(collection) = collection else {
        return Err(Error::Refused(format!(
            "twitch: collection {id}: the API answers no collection; it was deleted, or it is \
             the kind of page Twitch has retired — a channel's current videos are \
             https://www.twitch.tv/<name>/videos"
        )));
    };

    let title = text(collection, &["title"]).unwrap_or_else(|| id.to_string());
    let edges = at(collection, &["items", "edges"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for edge in &edges {
        // The edge type is stated by the API; a different one inside a
        // collection is not a video, and inventing a URL for it would be a
        // request to the wrong thing.
        if at(edge, &["__typename"]).and_then(Value::as_str) != Some("CollectionItemEdge") {
            continue;
        }
        if let Some(entry) = edge.get("node").and_then(entry_from) {
            entries.push(entry);
        }
    }
    if entries.is_empty() {
        return Err(Error::Refused(format!(
            "twitch: collection {id}: it lists no recordings; an empty collection and one whose \
             videos all expired answer the same way"
        )));
    }
    Ok(title)
}

/// One `CollectionItemEdge.node` as an entry. Only a numeric `id` is a VOD: a
/// clip URL has a different shape and is not something this build reads.
fn entry_from(node: &Value) -> Option<Entry> {
    let id = text(node, &["id"])?;
    if id.is_empty() || id.len() > 12 || !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let title = text(node, &["title"]).unwrap_or_else(|| format!("recording {id}"));
    let duration = at(node, &["lengthSeconds"])
        .and_then(Value::as_f64)
        .map(|seconds| seconds.round().max(0.0) as u64)
        .filter(|seconds| *seconds > 0);
    Some(Entry {
        url: format!("https://www.twitch.tv/videos/{id}"),
        id,
        title,
        duration,
    })
}

/// The first `errors[].message` anywhere in the batch, which is how GraphQL
/// reports a bad hash or an unknown operation.
fn first_error(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(errors) = map.get("errors").and_then(Value::as_array) {
                if let Some(message) = errors
                    .iter()
                    .find_map(|error| error.get("message").and_then(Value::as_str))
                {
                    return Some(message.to_string());
                }
            }
            map.values().find_map(first_error)
        }
        Value::Array(items) => items.iter().find_map(first_error),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("a URL")
    }

    fn answer() -> Value {
        json!([{
            "data": {
                "collection": {
                    "title": "Playthrough Archives",
                    "items": {
                        "edges": [
                            { "__typename": "CollectionItemEdge", "node": {
                                "__typename": "Video",
                                "id": "1234567890",
                                "title": "Part one",
                                "lengthSeconds": 7200
                            }},
                            { "__typename": "CollectionItemEdge", "node": {
                                "__typename": "Video",
                                "id": "987654321",
                                "title": "Part two"
                            }},
                            // A clip inside a collection is not a VOD.
                            { "__typename": "CollectionItemEdge", "node": {
                                "__typename": "Clip", "id": "SomeClipSlug"
                            }}
                        ]
                    }
                }
            }
        }])
    }

    #[test]
    fn a_collection_page_is_a_list_and_a_vod_is_not() {
        assert!(matches!(
            kind(&url("https://www.twitch.tv/collections/o9zZer3IQBhTJw")),
            Some(Kind::Collection(id)) if id == "o9zZer3IQBhTJw"
        ));
        assert!(kind(&url("https://www.twitch.tv/videos/1234567890")).is_none());
        assert!(kind(&url("https://www.twitch.tv/someone")).is_none());
        assert!(
            kind(&url("https://www.twitch.tv/collections/")).is_none(),
            "a collections tab with no id is not a collection"
        );
    }

    #[test]
    fn the_batch_becomes_numbered_vod_urls_with_titles_and_lengths() {
        let mut entries = Vec::new();
        let title = parse(&answer(), "o9zZer3IQBhTJw", &mut entries).expect("a collection parses");
        assert_eq!(title, "Playthrough Archives");
        assert_eq!(entries.len(), 2, "a clip is skipped, not turned into a VOD");
        assert_eq!(entries[0].url, "https://www.twitch.tv/videos/1234567890");
        assert_eq!(entries[0].title, "Part one");
        assert_eq!(entries[0].duration, Some(7200));
        assert_eq!(entries[1].duration, None);
    }

    #[test]
    fn a_collection_that_no_longer_exists_is_refused_by_name() {
        let answer = json!([{ "data": { "collection": null } }]);
        let mut entries = Vec::new();
        let error = parse(&answer, "gone", &mut entries).expect_err("nothing to read");
        let text = error.to_string();
        assert!(text.contains("no collection"), "{text}");
        assert!(text.contains("deleted"), "{text}");
    }

    #[test]
    fn an_empty_collection_is_a_refusal_rather_than_an_empty_list() {
        let answer =
            json!([{ "data": { "collection": { "title": "Empty", "items": { "edges": [] } } } }]);
        let mut entries = Vec::new();
        let error = parse(&answer, "empty", &mut entries).expect_err("nothing listed");
        assert!(error.to_string().contains("lists no recordings"), "{error}");
    }

    #[test]
    fn an_api_error_is_quoted_rather_than_swallowed() {
        let answer = json!([{ "errors": [{ "message": "PersistedQueryNotFound" }] }]);
        let mut entries = Vec::new();
        let error = parse(&answer, "1", &mut entries).expect_err("the hash is stale");
        assert!(
            error.to_string().contains("PersistedQueryNotFound"),
            "{error}"
        );
    }
}
