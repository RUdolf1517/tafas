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

A **password-protected** video is two steps, not one, because the first answer is
an empty shell: the config comes with `"view": 4` and no files at all.

| Step | Request |
| --- | --- |
| 1 | the config, as above — `"view": 4` means locked |
| 2 | `POST <video.redirect_url>/check-password`, body `password=<base64 of the password>`, form-encoded, with the player page as the referer |

A correct password answers the whole config, so nothing is fetched twice. The
password comes from `TAFAS_VIDEO_PASSWORD`, or from the first line of the file
`TAFAS_VIDEO_PASSWORD_FILE` names — never from an argument, because `--help` is
pasted into bug reports and an argument is visible in `ps`. A wrong password is
`418`, which is reported as "not accepted" and not retried.

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

## 3. Albums and showcases

An album (`/album/<id>`) and a showcase (`/showcase/<id>` or
`/showcase/<slug>`) are pages that name other videos, so they are read as a
**list**, not as a stream: each entry is then captured on its own, exactly as a
YouTube playlist entry is ([cli §3](../cli.md#3-selection)).

| Step | Request |
| --- | --- |
| 1 | `https://vimeo.com/_next/viewer` — the web player's short-lived `jwt`; nothing logs in |
| 2 | `https://api.vimeo.com/albums/<id>?fields=name,description,privacy,user.name` — the title, and whether the album is public |
| 3 | `https://api.vimeo.com/albums/<id>/videos?fields=link,uri,name,duration&per_page=100&page=N` |

A password-protected album adds one step between 2 and 3:
`POST https://vimeo.com/showcase/<id>/auth` with `password` and the viewer's
`xsrft` token, form-encoded. Its answer's `hashed_pass` travels on **every**
following `videos` request as `_hashed_pass`; a page fetched without it is a
`400`, which reads exactly like the end of the album.

A non-numeric showcase address is resolved to its id first, through
`https://vimeo.com/showcase/<slug>/auth` — the endpoint the page itself calls.
The `jwt` is asked for rather than replaced by a client secret: no credential is
embedded in this binary (see [§4](#4-refused)).

An entry's URL comes from its `uri` (`/videos/76979871` → `https://vimeo.com/76979871`)
and not from its `link`, because an album may hold a video under a custom
address that the video reader would have to guess about.

| Situation | Result |
| --- | --- |
| the album is protected by a password, and none was given | refusal (1) naming the album and both password variables |
| the album's password is wrong | refusal (1): the endpoint answers `401`, nothing is retried |
| `_next/viewer` answers no `jwt` | refusal: the album cannot be read without a session |
| the album answers `404` | refusal: gone, private, or an account `--cookies FILE` might reach |
| the album lists no videos | refusal naming the album, as an empty list is never a success |
| more than two pages of entries (about 200) | refusal stating the count it read, where it stopped |
| a page that brings no new entries | the walk ends there, so a stale `paging` block cannot loop |

## 4. Refused

| Situation | Result |
| --- | --- |
| `404` on the config endpoint | refusal (1): gone, private, or unlisted without its hash |
| no progressive file and no playlist | refusal (1): private or blocked from embedding; `--cookies FILE` is sent when named, no challenge is solved |
| `"view": 4` and no password given | refusal (1) naming `TAFAS_VIDEO_PASSWORD` and the file variant, and saying why there is no flag |
| `"view": 4` and the password is wrong | refusal (1): the player answers `418`, nothing is retried and nothing is guessed |
| `"view": 4` and no `video.redirect_url` | refusal (1): there is no page to prove the password against |
| `request.files.dash` and nothing else | refusal (1) naming DASH and `SegmentBase`: this build does not read it |
| a `progressive` entry whose `mime` is not MP4 | dropped, not converted |

**What it cannot reach:** DASH-only videos (that is what `SegmentBase` would
need), and anything that requires solving a challenge — a captcha, a login, a
proof of work. A password is the one exception, and it is only the password:
nothing is sent on the user's behalf except the one value they supplied. There is
no fallback that pretends otherwise.
