# Vimeo

Hosts: vimeo.com, player.vimeo.com. Detection and the host rules are in
[extractors §1](../extractors.md#1-detection).

Related: [extractors](../extractors.md) · [cli](../cli.md) · [guarantees](../guarantees.md)

This page is the part of [extractors](../extractors.md) that is about Vimeo:
its routes, what is selected, and every refusal, with the sentence it produces.
The overview keeps what applies to every site — detection, what a refusal looks
like when it happens, what happens when a site changes, and the
`--extractor-args` escape hatch — so this page can be read on its own.

## 1. Route

One request: `https://player.vimeo.com/video/<id>/config`, the JSON the embed
player asks for itself, with a `vimeo.com` referer and origin because the player
checks them. The document lists `request.files.progressive[]` — complete MP4s,
picture and sound in one stream — and `request.files.hls.cdns[*].url` when the
video has a playlist. No HTML is parsed: the page is a player, and the player
already knows what it would play.

An unlisted video needs the hash that its URL carries (`/76979871/1a2b3c4d5e`,
or `?h=` on the player host), and the hash is passed back as `?h=`.

## 2. What is selected

| In the document | Becomes |
| --- | --- |
| `request.files.progressive[].url` | a `Mp4` format, `vimeo-<quality>`, with its height, fps and bitrate |
| `request.files.hls.cdns[*].url` | one `Hls` format, `vimeo-hls`; the Google origin first, because it answers without a token |
| `video.thumbs` | the poster, widest key wins (`--write-thumbnail`) |
| `video.title`, `video.owner.name`, `video.duration` | the name, the uploader, the duration |

Progressive files are kept *beside* the playlist rather than instead of it. The
playlist states no size, so selection prefers a progressive MP4 by itself, and
`--format vimeo-hls` can still ask for the playlist when that is what the user
wants.

## 3. Refused

| Situation | Result |
| --- | --- |
| `404` on the config endpoint | refusal (1): gone, private, or unlisted without its hash |
| no progressive file and no playlist | refusal (1): password-protected, private or blocked from embedding; `--cookies FILE` is sent when named, no challenge is solved |
| `request.files.dash` and nothing else | refusal (1) naming DASH and `SegmentBase`: this build does not read it |
| a `progressive` entry whose `mime` is not MP4 | dropped, not converted |

**What it cannot reach:** DASH-only videos (that is what `SegmentBase` would need),
videos behind a password, and anything that requires solving a challenge. There is
no fallback that pretends otherwise.
