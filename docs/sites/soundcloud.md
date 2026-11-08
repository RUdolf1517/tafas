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

## 3. Refused

| Situation | Result |
| --- | --- |
| a set (`/<artist>/sets/<slug>`) | refusal naming it: a set is a list of tracks, and one URL is one file |
| no `__sc_hydration` | refusal (1): a consent wall, a block on this address, or not a track |
| no page `client_id` | refusal (1), naming `--cookies FILE` |
| only HLS/opus transcodings | refusal (1): this build demuxes neither opus nor a fragmented audio playlist |
| no progressive transcoding | refusal (1): a Go+ track, or only a preview of one |
| the transcoding answers no `url` | refusal (1), which is what a region block looks like |

**What it cannot reach:** Go+ tracks, opus-only renditions, sets, and anything
behind a consent wall. An MP3 is also the one written file that is **not**
structurally verified — there is no container to check, so the byte count is the
whole guarantee.
