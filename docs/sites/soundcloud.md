# SoundCloud

Hosts: soundcloud.com, on.soundcloud.com. Detection and the host rules are in
[extractors §1](../extractors.md#1-detection).

Related: [extractors](../extractors.md) · [cli](../cli.md) · [guarantees](../guarantees.md)

This page is the part of [extractors](../extractors.md) that is about SoundCloud:
its routes, what is selected, and every refusal, with the sentence it produces.
The overview keeps what applies to every site — detection, what a refusal looks
like when it happens, what happens when a site changes, and the
`--extractor-args` escape hatch — so this page can be read on its own.

## 1. Route

Three requests, and each one is needed:

1. the track page, for `window.__sc_hydration` — the array the site's own player
   renders from. The entry with `"hydratable":"sound"` carries the title, the
   uploader, the duration, the artwork and `media.transcodings`;
2. the transcoding URL, with the page's own public `client_id` appended. It
   answers a small JSON document, not audio;
3. the CDN URL that document names — the bytes.

`on.soundcloud.com` is a redirector, so one redirect is followed first. The
`client_id` is scraped from the page (`client_id=<32 alphanumerics>`, in a script
URL or inline JSON), never guessed and never hard-coded.

## 2. What is selected

| In the document | Becomes |
| --- | --- |
| the first `transcodings[]` entry with `protocol: "progressive"` and `mime_type: "audio/mpeg"` | the stream, as an **MP3** copied verbatim |
| `title`, `user.username`, `duration`, `artwork_url` | the name, the uploader, the duration, the poster |

One format, no selection to make: a track is one file. `--audio-only` is
therefore implied rather than required, and `--quality` has nothing to choose
between.

## 3. A set

A set (`/<artist>/sets/<slug>`) is the site's own name for a playlist of
tracks, and it is read as a **list page** — `--playlist-items`, one file per
entry, `01 - Title [id].mp3`, and one dead track costing the others nothing,
exactly as for a YouTube playlist.

1. the set's page, for the same hydration document. The entry with
   `"hydratable":"playlist"` carries the title, the uploader, `id`, `track_count`
   and the first few `tracks`;
2. `https://api-v2.soundcloud.com/playlists/<id>?client_id=…&limit=200&linked_partitioning=true`,
   the request the site's own player makes, for the rest. Its `next_href` is
   followed as it arrived, with the page's `client_id` appended when the href
   does not carry one. `secret_token` is sent only when the page itself had one.

Every track that has a `permalink_url` becomes an entry, and each entry is read
later by the track route above, on its own. A track the page lists without an
address is a stub the site will not serve; it is skipped rather than requested.

**A short set is not a whole one.** The page says how many tracks the set holds
(`track_count`). When fewer entries could be read — the API refused a page, or
the set really is missing one — the listing is still captured and the run says
so on stderr, before the first entry:

```
tafas: soundcloud 1234: the page says it holds 40 entries and this build read 12;
the 12 entries read are captured, and nothing here claims the list is whole
```

If the page never said a count and the API will not answer, that is a refusal
rather than a capture: nothing in the run could tell whether the list is whole.

Paging is bounded at two pages of two hundred, the same bound the other list
readers use. A set longer than that is refused with both numbers — how many were
read, and which flag to use — not downloaded in part.

## 4. Refused

| Situation | Result |
| --- | --- |
| no `__sc_hydration` | refusal (1): a consent wall, a block on this address, or not a track |
| no page `client_id` | refusal (1), naming `--cookies FILE` |
| only HLS/opus transcodings | refusal (1): this build demuxes neither opus nor a fragmented audio playlist |
| no progressive transcoding | refusal (1): a Go+ track, or only a preview of one |
| the transcoding answers no `url` | refusal (1), which is what a region block looks like |
| a set page with no hydration document | refusal (1), the same sentence with “set” in it |
| a set page with no playlist entry | refusal (1): the document lists neither a playlist entry nor tracks |
| a set whose pages hold no readable track | refusal naming it, as an empty list is never a success |
| a set longer than two pages of two hundred | refusal giving the number read and naming `--playlist-items` |

**What it cannot reach:** Go+ tracks, opus-only renditions, a private set whose
token the page does not carry, and anything behind a consent wall. An MP3 is also the one written file that is **not**
structurally verified — there is no container to check, so the byte count is the
whole guarantee.
