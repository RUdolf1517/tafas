# Reddit

Hosts: reddit.com, old.reddit.com, redd.it, v.redd.it. Detection and the host rules are in
[extractors §1](../extractors.md#1-detection).

Related: [extractors](../extractors.md) · [cli](../cli.md) · [guarantees](../guarantees.md)

This page is the part of [extractors](../extractors.md) that is about Reddit:
its routes, what is selected, and every refusal, with the sentence it produces.
The overview keeps what applies to every site — detection, what a refusal looks
like when it happens, what happens when a site changes, and the
`--extractor-args` escape hatch — so this page can be read on its own.

## 1. Route

`/comments/<id>.json?raw_json=1` for a post — the listing the old mobile clients
asked for; its first child is the post, and `secure_media.reddit_video` is the
video. A bare `v.redd.it/<hash>` needs no JSON at all: the master playlist lives
at a fixed path under the same hash, so the URL is built.

## 2. One route, and why not the two obvious ones

Only `hls_url` is taken, and the two alternatives in the same object are named
here rather than silently skipped:

| Field | Why it is not offered |
| --- | --- |
| `fallback_url` (`DASH_<height>.mp4`) | it is a **video-only** MP4: no audio track at all. Writing it as a finished video would be a file with a picture and no sound |
| `dash_url` (`DASHPlaylist.mpd`) | DASH `SegmentBase`, which this build does not read |
| `hls_url` | a master playlist with the video renditions and an `EXT-X-MEDIA` audio rendition, which `hlsfetch` follows and joins into one MP4 — **taken** |

A `v.redd.it` GIF is the same route, and comes out with no audio because the
source has none.

## 3. Refused

| Situation | Result |
| --- | --- |
| `404` on the post listing | refusal (1): deleted, private, or a community that is gone |
| `removed_by_category` present | refusal (1) naming the category |
| no `reddit_video` | refusal (1): a text, link, gallery or photo post is not a video |
| a video with no `hls_url` | refusal (1) that says the progressive files carry no audio, so no silent file is written |
| `over_18` and nothing playable | refusal (1) that names `--cookies FILE`; nothing challenges a wall |

**What it cannot reach:** videos behind a wall or a private community (a session
may help), and anything the post lists only as DASH.
