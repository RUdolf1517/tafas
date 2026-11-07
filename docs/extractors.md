# Extractors: what each site gives, and what is refused

Related: [cli](cli.md) · [guarantees](guarantees.md) · [roadmap](roadmap.md)

This document is the promise, not the aspiration. Every "refused" entry below is
a live code path with a test, and the refusal names itself in the output so a
user can tell three very different situations apart: *the site changed*, *the
content needs an account*, and *this build chooses not to*.

The general rule: **a stream is used only if it arrived with a URL that works.**
A format that arrives encrypted, ciphered, or as a playlist this build cannot
rebuild is dropped, and if nothing survives, the run fails with the reason
rather than with an empty file.

---

## 1. Detection

| Host | Reader |
| --- | --- |
| `youtube.com`, `music.youtube.com`, `youtube-nocookie.com`, `youtu.be` | YouTube |
| `tiktok.com`, `vm.tiktok.com`, `vt.tiktok.com` | TikTok |
| `x.com`, `twitter.com`, `t.co` | X |
| `vimeo.com`, `player.vimeo.com` | Vimeo |

`m.` and `mobile.` prefixes are stripped before the comparison, and so is a
trailing dot. `notyoutube.com` and `youtube.com.evil.example` are **not**
YouTube: the comparison is on the whole host, which is why a lookalike is a
`unsupported` error rather than a request to somebody else's server.

## 2. YouTube

### 2.1 Routes, in order

1. **The watch page.** `GET /watch?v=<id>&hl=en&has_verified=1` with a desktop
   user agent and the consent cookies (`CONSENT=YES+cb; SOCS=CAI`). The
   `ytInitialPlayerResponse` object inlined in the HTML is the same document the
   browser receives, so it is parsed directly (brace-matched, string-aware, so a
   `}` inside a title cannot truncate it). The page also supplies
   `INNERTUBE_API_KEY`, which the next route needs.
   The page's `INNERTUBE_API_KEY` is what route 2 authenticates with. When the
   page cannot be read at all, the key in the source (`PUBLIC_WEB_API_KEY`) is
   used: it is YouTube's own web-client key, public, in every page, and a
   secret scanner will call it a leak. [SECURITY.md § Credentials in the
   source](../SECURITY.md#credentials-in-the-source-and-what-to-do-when-a-scanner-flags-one)
   explains why it stays.
2. **InnerTube**, `POST /youtubei/v1/player`, with the clients below in this
   order. Each is a different `context.client`, because YouTube serves different
   documents to different clients, and the difference is exactly whether a
   format needs its signature solved.

| Order | Client | Why it is here |
| --- | --- | --- |
| 1 | `ANDROID_VR` (Quest 3, Android 12) | tends to answer with directly playable URLs and no PO token |
| 2 | `IOS` | same, different fingerprint |
| 3 | `ANDROID` | the historical fallback |
| 4 | `TVHTML5_SIMPLY_EMBEDDED_PLAYER` | the embedded player, which some videos allow when others do not |
| 5 | `WEB` | last, because it is the most likely to demand a challenge |

The first response that yields at least one directly playable format wins. The
formats remember which client produced them, and `--list-formats` shows it in
`-v` output.

### 2.2 What is selected

`streamingData.formats` (progressive: picture and sound together, up to 720p)
and `streamingData.adaptiveFormats` (DASH: video-only and audio-only, up to the
source maximum). A stream is classified by what it carries: audio if its MIME
type is `audio/*` or it reports `audioQuality`; video if it is `video/*` or
reports a width.

`-Q best` prefers a DASH video stream plus an m4a audio stream and remuxes,
because that is the only way to reach 1080p and above. A progressive stream is
used when no pair exists.

### 2.3 Refused: signature cipher

A format may arrive with `signatureCipher` instead of `url`. Playing it means
running the site's own minified player JavaScript to transform the `s` parameter
and the `n` throttle parameter. This build does not execute JavaScript, so such
formats are **dropped**. If every format of a video is protected that way:

```
tafas: youtube: dQw4w9WgXcQ: every format is signature-protected; this build does not
execute the player JavaScript that would decipher it (docs/extractors.md §4)
```

Exit 1. The client chain is what usually avoids this in practice; when it does
not, the refusal is honest rather than a guess.

### 2.4 Refused: sign-in check, age gate

A `playabilityStatus` other than `OK` is reported with its own reason. The two
that matter:

| Status / reason | Output |
| --- | --- |
| `LOGIN_REQUIRED` + "Sign in to confirm you're not a bot" | refusal naming the check; cookies from `--cookies` are sent, no challenge is solved |
| `AGE_VERIFICATION_REQUIRED` / "inappropriate for some users" | refusal naming the age gate |
| `UNPLAYABLE`, `ERROR`, region block | refusal quoting the site's own sentence |

v0.1.0 had no `--cookies` flag, on the grounds that a cookie jar turns a
stateless command into something that can act as a logged-in account. v0.2 adds
one with that cost stated: the file is read **once**, sent only to the hosts its
domains match, never written back, and never logged. tafas still has no sign-in
flow and stores nothing between runs — the file is the user's, and using it is
the user's decision. Details in [safety §5](safety.md#5-network).

### 2.5 Captions, and the languages a video does not have

`--write-subs` writes WebVTT, one file per selected track:
`<name>.<language>.vtt`, or `<name>.<language>.auto.vtt` when the site
transcribed the audio rather than a person writing the cues. The two are never
merged and never presented as the same thing, because one of them is usually
wrong in the places that matter.

The tracks come from `captions.playerCaptionsTracklistRenderer.captionTracks`,
and each `baseUrl` is asked for `fmt=vtt` at parse time — the format is decided
in one place, not at fetch time.

| Situation | Output |
| --- | --- |
| `--sub-langs` names a track the video does not have | refusal (exit 1) naming the languages it does have, or `none` |
| `--sub-langs` with a `*` that matches several tracks | all of them, one file each |
| the video lists no captions at all, no `--sub-langs` given | a note on stderr, exit 0: the video is what was asked for and it is on disk |
| `timedtext` answers `200` with an empty body | refusal naming the route, not the language: YouTube does this to some networks |
| `timedtext` answers with something that is not WebVTT | refusal stating how many bytes arrived |
| `--sub-langs` without `--write-subs` | usage error (exit 2), before any request |

A track is a reference until the flag asks for it: nothing is fetched otherwise,
and `--list-formats` does not print them.

### 2.6 Live streams

A live stream without a finished recording has no fixed `mdat` to fetch; the
manifest is a rolling list of fragments. This build does not stitch fragments,
so a live URL fails with `unsupported`. A finished stream that the site has
already published normally works.

## 3. TikTok

### 3.1 Routes

1. **The page.** `GET /@user/video/<id>` and the JSON inlined as
   `__UNIVERSAL_DATA_FOR_REHYDRATION__`, read at
   `__DEFAULT_SCOPE__["webapp.video-detail"].itemInfo.itemStruct`.
2. **The mobile feed.** `api22-normal-c-useast2a.tiktokv.com/aweme/v1/feed/`
   with the app user agent, read at `aweme_list[0]`. Used when the page comes
   back as a verification wall or without inline state.

Short links (`vm.tiktok.com`, `vt.tiktok.com`) are resolved once, and the final
URL supplies the id. `/photo/<id>` posts are images, not video.

### 3.2 Watermark

Both the page and the feed hand out several URLs for the same video. `playAddr`
is preferred, and `downloadAddr` is kept as a fallback, because the first is the
stream the player shows and the second is often the version with a burned-in
attribution. Neither is re-encoded: whatever TikTok serves is what is written.

### 3.3 Refused

| Situation | Output |
| --- | --- |
| verification wall on both routes | refusal naming both routes' failures |
| private or deleted post | `unsupported` with the site's status message |
| region-restricted post | refusal quoting the status message |
| photo post | images are downloaded; a photo post with no video is not an error |

TikTok names no caption tracks, so `--write-subs` there is a note rather than a
file, and `--sub-langs` names a language the site cannot offer: a refusal that
says so, because guessing would be a quiet lie.

## 4. X

### 4.1 Route

The syndication endpoint that the embedded-tweet widget uses:
`cdn.syndication.twimg.com/tweet-result?id=<id>&lang=en`. It returns the post as
JSON: `text`, `user.screen_name`, and `mediaDetails[]` with `video_info.variants`
for video and `media_url_https` for photos.

Variants whose `content_type` is `video/mp4` are candidates; the highest
`bit_rate` wins. An animated GIF arrives as an MP4 variant with no audio track
and is written as `.mp4`, which is what it is.

A variant whose `content_type` is `application/x-mpegURL` is an HLS playlist.
When a post offers nothing else, the playlist is walked, the segments are
demuxed and the result is written as an MP4 in the same process — no transcoding
and no external binary, exactly like the remux path. Segments encrypted with
`AES-128` are decrypted on the way in, with the key fetched once. A progressive MP4 is still
preferred when both shapes exist: one request instead of one per segment.

### 4.2 Photos

A post with photos and no video downloads the photos, one file each, named
`<stem>-1.jpg`, `<stem>-2.jpg` … A post with video *and* photos downloads the
video; use `--list-formats` to see both.

### 4.3 Refused

| Situation | Output |
| --- | --- |
| only a fragmented-MP4 playlist (`EXT-X-MAP`) | read, since v0.3: the header is fetched once and the fragments are stitched ([hls §2](hls.md#2-two-containers-one-writer)) |
| a `SAMPLE-AES` playlist | refusal at parse time: the cipher is inside the container's framing |
| tombstone (deleted, protected, age-restricted) | refusal naming the missing account |
| post with no media at all | `unsupported` |
| id not found by syndication | `unsupported`, because the endpoint answers 404 for old and private posts alike |
| `--write-subs` | a note: X carries no caption tracks |

The X refusal table stays three rows long on purpose: a post either has media or
it does not, and a caption flag cannot change that.

## 5. Vimeo

### 5.1 Route

One request: `https://player.vimeo.com/video/<id>/config`, the JSON the embed
player asks for itself, with a `vimeo.com` referer and origin because the player
checks them. The document lists `request.files.progressive[]` — complete MP4s,
picture and sound in one stream — and `request.files.hls.cdns[*].url` when the
video has a playlist. No HTML is parsed: the page is a player, and the player
already knows what it would play.

An unlisted video needs the hash that its URL carries (`/76979871/1a2b3c4d5e`,
or `?h=` on the player host), and the hash is passed back as `?h=`.

### 5.2 What is selected

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

### 5.3 Refused

| Situation | Result |
| --- | --- |
| `404` on the config endpoint | refusal (1): gone, private, or unlisted without its hash |
| no progressive file and no playlist | refusal (1): password-protected, private or blocked from embedding; `--cookies FILE` is sent when named, no challenge is solved |
| `request.files.dash` and nothing else | refusal (1) naming DASH and `SegmentBase`: this build does not read it |
| a `progressive` entry whose `mime` is not MP4 | dropped, not converted |

**What it cannot reach:** DASH-only videos (that is what `SegmentBase` would need),
videos behind a password, and anything that requires solving a challenge. There is
no fallback that pretends otherwise.

## 6. What a refusal looks like, and what it does not

A refusal is a **failure**: exit 1, one line on stderr, and — under `--json` —
an object with `"status": "failed"` and a `reason`. It is not a skip that exits
0, because a user who asked for one file and got none has not succeeded.

Nothing is ever written on a refusal. No empty file, no directory, no truncated
`.mp4` with a valid header: the extractors run before the first byte of media is
requested, so a refusal costs one or two page requests and no disk.

## 7. When a site changes

Every extractor is written to *fail* rather than to produce something wrong:
`read_player` returns "no formats" instead of panicking on a missing field, the
JSON lookup helpers are total, and media URLs are used only when they are
strings. A changed page therefore produces a refusal with the status the site
reported, which is the single most useful thing a bug report can contain.

## 8. The escape hatch: `--extractor-args`

Section 6 ends with "a changed page produces a refusal", and a refusal is useful
but not a download. The flag for the day between the change and the release is
`--extractor-args SITE:KEY=VALUE`, and it is deliberately the smallest thing that
could work.

There are two keys, both of them routes this build already has:

| value | what it does | when it helps |
| --- | --- | --- |
| `youtube:client=NAME` | asks one InnerTube client and drops the rest of the chain (`android_vr`, `ios`, `android`, `tv_embedded`, `web`) | one client still answers while the default order trips a bot check |
| `tiktok:route=web` | reads the page only, never the mobile feed | the page works and the feed 403s, so the fallback wastes a request |
| `tiktok:route=app` | asks the feed only, never the page | the desktop page serves a verification wall and the feed does not |

Three rules make it an escape hatch rather than a configuration language. The
vocabulary is **closed**: a site's keys, and each key's values, are constants in
`src/extractor_args.rs`, and `--capabilities` publishes the same lists, so the
set of values a release accepts is documented where a script can read it. It is
**checked before the first request**, so `youtube:client=flash` is exit 2 with
`android_vr|ios|android|tv_embedded|web` in the message, not a download that
quietly used the default. And it **only narrows**: there is no value that turns
on a route the build does not already try by default, so a pinned client is a
subset of the chain, and the failure for a pinned route names the pin — "no
other client was tried" is the difference between a site problem and a
self-inflicted one.

A pinned route is never silent in the other direction either: dropping a route
puts a note in the failure that names the flag, and `youtube:client=NAME` skips
the watch page instead of asking it and discarding the answer.

If a site changes and tafas writes a file that a player rejects, that is the
highest-priority bug class in this project. [SECURITY.md](../SECURITY.md) covers
the private channel for anything that looks like more than a compatibility
problem.
