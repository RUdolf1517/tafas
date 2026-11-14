# Twitch

Hosts: twitch.tv. Detection and the host rules are in
[extractors §1](../extractors.md#1-detection).

Related: [extractors](../extractors.md) · [cli](../cli.md) · [guarantees](../guarantees.md)

This page is the part of [extractors](../extractors.md) that is about Twitch:
its routes, what is selected, and every refusal, with the sentence it produces.
The overview keeps what applies to every site — detection, what a refusal looks
like when it happens, what happens when a site changes, and the
`--extractor-args` escape hatch — so this page can be read on its own.

## 1. Route

One batched GraphQL request carries two operations:

1. `PlaybackAccessToken`, asked by persisted-query hash with the **public web
   client id** in a `client-id` header. It answers `value` and `signature`;
2. `video(id:)` for the title, the uploader, the length and the poster — so a
   capture does not pay a second round trip just to name the file.

Those two values become the playlist URL:
`https://usher.ttvnw.net/vod/<id>.m3u8?...&nauth=<token>&nauthsig=<signature>`.
`value` is a JSON document, so it is percent-encoded before it goes in the query
string; `signature` is a signature over it and is sent as it arrived. `hlsfetch`
then assembles the playlist like any other — the renditions are MPEG-TS, and the
audio-only variant is offered by the same master list.

## 2. Clips

A clip (`clips.twitch.tv/<Slug>`, its `embed?clip=` form,
`twitch.tv/<channel>/clip/<Slug>`, and the `m.` host) has no playback token and
no usher playlist. One unbatched GraphQL request answers with everything:

| Body | Answer |
| --- | --- |
| `query VideoAccessToken_Clip($slug: ID!)`, `operationName` and the public web client id in a header | `clip.playbackAccessToken { value, signature }` and `clip.videoQualities[] { frameRate, quality, sourceURL }`, plus `thumbnailURL`, `durationSeconds` and `broadcaster.displayName` |

Each `sourceURL` is a **complete MP4** — picture and sound already joined — so
the chosen rendition is copied verbatim, through the same route an ordinary
progressive MP4 takes. Nothing is remuxed and nothing is assembled.

The URL is `sourceURL` plus `?sig=<signature>&token=<percent-encoded value>`,
and the qualities are offered best first: `quality` is a string (`"1080"`,
`"720"`, `"source"`), and it becomes the height when it is a number. A rendition
with an empty `sourceURL` is skipped rather than offered and refused later.

## 3. Collections

A collection (`/collections/<id>`) is a group of recordings a channel picked by
hand. It has no playback token of its own: it is a **list**, and each item is a
VOD that this page's first route reads later, on its own
([cli §3](../cli.md#3-selection)).

One request, against the same GraphQL endpoint:

| Body | Answer |
| --- | --- |
| `[{"operationName": "CollectionSideBar", "variables": {"collectionID": "<id>"}}]` by persisted-query hash | `data.collection.title`, and `items.edges[].node` — each node a video with `id`, `title`, `lengthSeconds` |

The public web client id goes in a header, as it does for a VOD. An item's URL
is built from its numeric id, so a clip that a collection happens to hold is
skipped rather than turned into a request to the wrong thing.

A channel's collections *tab* (`/<name>/videos?filter=collections`) is
deliberately not read: its entries are collections, and `--playlist-items` on a
list of lists is the nested-list refusal one level up. The URL of a collection
is what this build takes.

| Situation | Result |
| --- | --- |
| the API answers no collection | refusal: deleted, or a page Twitch has retired — the channel's current videos are given as the alternative |
| the collection lists no recordings | refusal naming it, as an empty list is never a success |
| the API quotes an error (a stale persisted-query hash) | refusal quoting the message |

## 4. Refused

| Situation | Result |
| --- | --- |
| a channel URL (`twitch.tv/<name>`) | unsupported (1): a live stream has no end, and a file needs one |
| `streamPlaybackAccessToken` null | refusal (1): expired, deleted, or subscriber-only; `--cookies FILE` is named |
| a token without `value` or without `signature` | refusal (1) naming which field is missing |
| an `errors` array in the answer | refusal (1) quoting the API's own message, because a changed persisted-query hash arrives exactly that way |
| a clip slug the API does not know | refusal naming both shapes that work: `clips.twitch.tv/<slug>` and `/clip/` |
| every clip rendition still encoding | refusal: “being encoded”, not an empty file |
| a clip answer without `sig` or `token` | refusal naming the missing half |
| an address that is neither a VOD, a clip nor a collection | usage (2): both accepted shapes are quoted |

**What it cannot reach:** live channels, subscriber-only VODs without a
session, and anything Twitch has already expired. Clips are read, but only the
ones still on the site: an expired clip is gone for every client. The client id is public and is not a credential — `SECURITY.md` says so
next to YouTube's key.
