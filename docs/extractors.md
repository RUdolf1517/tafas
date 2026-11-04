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
| `LOGIN_REQUIRED` + "Sign in to confirm you're not a bot" | refusal naming the check; no cookies are sent and none are accepted as an argument |
| `AGE_VERIFICATION_REQUIRED` / "inappropriate for some users" | refusal naming the age gate |
| `UNPLAYABLE`, `ERROR`, region block | refusal quoting the site's own sentence |

There is no `--cookies` flag. That is a deliberate boundary of v0.1.0, not an
oversight: a cookie jar turns a stateless command into something that can act as
a logged-in account, and that decision (and its failure modes) is on the
[roadmap](roadmap.md#3-not-planned-for-v01) rather than smuggled in.

### 2.5 Live streams

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

## 4. X

### 4.1 Route

The syndication endpoint that the embedded-tweet widget uses:
`cdn.syndication.twimg.com/tweet-result?id=<id>&lang=en`. It returns the post as
JSON: `text`, `user.screen_name`, and `mediaDetails[]` with `video_info.variants`
for video and `media_url_https` for photos.

Variants whose `content_type` is `video/mp4` are candidates; the highest
`bit_rate` wins. An animated GIF arrives as an MP4 variant with no audio track
and is written as `.mp4`, which is what it is.

### 4.2 Photos

A post with photos and no video downloads the photos, one file each, named
`<stem>-1.jpg`, `<stem>-2.jpg` … A post with video *and* photos downloads the
video; use `--list-formats` to see both.

### 4.3 Refused

| Situation | Output |
| --- | --- |
| only `application/x-mpegURL` variants | refusal naming HLS; demuxing MPEG-TS and rebuilding sample tables is not in this build |
| tombstone (deleted, protected, age-restricted) | refusal naming the missing account |
| post with no media at all | `unsupported` |
| id not found by syndication | `unsupported`, because the endpoint answers 404 for old and private posts alike |

## 5. What a refusal looks like, and what it does not

A refusal is a **failure**: exit 1, one line on stderr, and — under `--json` —
an object with `"status": "failed"` and a `reason`. It is not a skip that exits
0, because a user who asked for one file and got none has not succeeded.

Nothing is ever written on a refusal. No empty file, no directory, no truncated
`.mp4` with a valid header: the extractors run before the first byte of media is
requested, so a refusal costs one or two page requests and no disk.

## 6. When a site changes

Every extractor is written to *fail* rather than to produce something wrong:
`read_player` returns "no formats" instead of panicking on a missing field, the
JSON lookup helpers are total, and media URLs are used only when they are
strings. A changed page therefore produces a refusal with the status the site
reported, which is the single most useful thing a bug report can contain.

If a site changes and tafas writes a file that a player rejects, that is the
highest-priority bug class in this project. [SECURITY.md](../SECURITY.md) covers
the private channel for anything that looks like more than a compatibility
problem.
