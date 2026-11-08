# YouTube

Hosts: youtube.com, music.youtube.com, youtube-nocookie.com, youtu.be. Detection and the host rules are in
[extractors §1](../extractors.md#1-detection).

Related: [extractors](../extractors.md) · [cli](../cli.md) · [guarantees](../guarantees.md)

This page is the part of [extractors](../extractors.md) that is about YouTube:
its routes, what is selected, and every refusal, with the sentence it produces.
The overview keeps what applies to every site — detection, what a refusal looks
like when it happens, what happens when a site changes, and the
`--extractor-args` escape hatch — so this page can be read on its own.

## 1. Routes, in order

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

## 2. What is selected

`streamingData.formats` (progressive: picture and sound together, up to 720p)
and `streamingData.adaptiveFormats` (DASH: video-only and audio-only, up to the
source maximum). A stream is classified by what it carries: audio if its MIME
type is `audio/*` or it reports `audioQuality`; video if it is `video/*` or
reports a width.

`-Q best` prefers a DASH video stream plus an m4a audio stream and remuxes,
because that is the only way to reach 1080p and above. A progressive stream is
used when no pair exists.

## 3. Refused: signature cipher

A format may arrive with `signatureCipher` instead of `url`. Playing it means
running the site's own minified player JavaScript to transform the `s` parameter
and the `n` throttle parameter. This build does not execute JavaScript, so such
formats are **dropped**. If every format of a video is protected that way:

```
tafas: youtube: dQw4w9WgXcQ: every format is signature-protected; this build does not
execute the player JavaScript that would decipher it (docs/extractors.md §3.3)
```

Exit 1. The client chain is what usually avoids this in practice; when it does
not, the refusal is honest rather than a guess.

## 4. Refused: sign-in check, age gate

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

## 5. Captions, and the languages a video does not have

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

## 6. Live streams

A live stream without a finished recording has no fixed `mdat` to fetch; the
manifest is a rolling list of fragments. This build does not stitch fragments,
so a live URL fails with `unsupported`. A finished stream that the site has
already published normally works.

## 7. Playlists and channels

A playlist or a channel page is not a video: it carries no player response, so
the watch-page route cannot read it. It is read by its own route, and what comes
back is a **list of entries** rather than a stream.

| URL | Read as |
| --- | --- |
| `youtube.com/playlist?list=PL…` | InnerTube `browse` with `browseId: VL<id>` |
| `youtube.com/@handle`, `/c/<name>`, `/user/<name>` | the page for its channel id, then `browse` of the channel's videos tab |
| `youtube.com/channel/UC…` | `browse` of that channel, then its videos tab |

What the run then does with the list:

- **Each entry is captured on its own**, into a directory, under
  `<position> - <title> [<id>].mp4`. The position is the position on the page, so
  `--playlist-items 3-5` writes `03`, `04`, `05` and `ls` sorts the way the
  browser does.
- **One dead entry does not stop the others.** Each failure is reported where it
  happens, the rest are still captured, and the run exits `1` — the same rule a
  URL list already followed.
- **`--playlist-items` selects**, with the grammar it uses for HLS segments; a
  selection that matches nothing is a refusal naming how many entries there are.
- **One level only.** A channel of playlists is refused rather than walked, and
  the message says which list it found.

Refused, by name, because each is a page this build cannot read honestly:

| Situation | Refusal |
| --- | --- |
| the page lists nothing (private, empty, members-only) | `lists no videos` |
| the list is longer than two browse pages | the count it did read, and where it stopped |
| a `lockupViewModel` entry (the newer shape) | not read yet: the entry is skipped, and a page of nothing but those is the "lists no videos" refusal above |
| a watch URL that also names a list (`watch?v=…&list=…`) | not a list: the video is what was named |
| a generated mix (`list=RD…`) | not a page anybody can read later |

Paging stops at two `browse` pages, about 200 entries. That is a chosen bound
rather than a technical one: a channel with ten thousand videos is not a capture
anybody wants to start by accident, and a refusal that says how many it read is
more useful than a download that runs for an hour.
