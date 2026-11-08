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
| `reddit.com`, `old.reddit.com`, `redd.it`, `v.redd.it` | Reddit |
| `soundcloud.com`, `on.soundcloud.com` | SoundCloud |
| `twitch.tv` | Twitch |

`m.` and `mobile.` prefixes are stripped before the comparison, and so is a
trailing dot. `notyoutube.com` and `youtube.com.evil.example` are **not**
YouTube: the comparison is on the whole host, which is why a lookalike is a
`unsupported` error rather than a request to somebody else's server.

## 2. The reader table, and a build with one site

Every site is a row in one table — `src/sites/registry.rs` — rather than a
`match` arm, a name in a list and a host in an enum. A row is a name, the hosts
it answers for, whether `--extractor-args` has anything to say to it, whether it
has *list* pages, and its readers. A row with list pages names one function of
shape `(Http, &Url, &str) -> Option<Result<Listing>>`: `None` means "not a list
page, ask the video reader", so an ordinary video URL never costs a request to
find that out, and a site without lists is a `None` in its row rather than a
branch in the dispatcher. Everything downstream is derived from that table: which URL goes to
which reader, what `--capabilities` prints under `sites`, and the sentence a
stranger host is refused with ("this build reads youtube, …"). Those three used
to be kept in step by hand, and a `packaging` test existed to notice when they
drifted; now there is nothing to keep in step.

**A build can leave a site out.** Each row sits behind a feature, and all seven
are on by default:

```sh
cargo build --no-default-features --features youtube     # a YouTube-only binary
cargo build --no-default-features --features youtube,twitch
```

That is not a runtime switch. A build without `vimeo` contains no Vimeo code —
not a route that refuses, and not a row that is skipped. The consequences are
the ones a user can see:

| What | A one-site build |
| --- | --- |
| `--capabilities` → `sites` | the sites it was built with |
| a URL for a site it does not have | `no extractor for …: this build reads youtube`, exit 1 |
| `--extractor-args` | only the keys of sites it has are accepted |
| the binary | smaller: the `features` job measures both and fails if the difference is not there |

Three of the seven rows have list pages, and each site's own page lists the
shapes: YouTube playlists and channels ([sites/youtube.md](sites/youtube.md)),
Vimeo albums and showcases ([sites/vimeo.md](sites/vimeo.md)), and Twitch
collections ([sites/twitch.md](sites/twitch.md)). A list is read into the same
`Listing` whatever the site, so `--playlist-items`, the directory rule, the
`03 - Title [id]` names and the per-entry independence are written once.

Nothing about this is a promise about a *release*: the published archives are
built with all seven, and a one-site binary is something a user builds for
themselves — which is exactly the audience for it, someone who wants a
downloader that cannot touch the other six hosts.

## 3. YouTube

Two routes — the watch page's inlined player response, then InnerTube with a fixed client order — plus a *list* route for playlists and channels. A ciphered format is dropped, never solved; a sign-in check is a refusal with its own sentence, because it needs `--cookies` rather than a retry.

Routes, what is selected, and every refusal with the sentence it produces: **[sites/youtube.md](sites/youtube.md)**.

## 4. TikTok

The page's rehydration document, then the mobile feed as a fallback. The watermark-free `playAddr` is preferred, and `--extractor-args tiktok:route` can drop either route.

Routes, what is selected, and every refusal with the sentence it produces: **[sites/tiktok.md](sites/tiktok.md)**.

## 5. X

One syndication endpoint per post, no HTML. A post with several photos is a list of images rather than a video, and a post whose media is ciphered is refused by name.

Routes, what is selected, and every refusal with the sentence it produces: **[sites/x.md](sites/x.md)**.

## 6. Vimeo

The player's own config endpoint: progressive MP4s and, when the video has one, an HLS rendition. DASH is `SegmentBase` and is refused rather than guessed at; an unlisted video needs the hash its URL carries.

Routes, what is selected, and every refusal with the sentence it produces: **[sites/vimeo.md](sites/vimeo.md)**.

## 7. Reddit

The post's JSON, and one media route only — `hls_url`. `fallback_url` is a video-only MP4 and `dash_url` is `SegmentBase`, and both refusals say so instead of writing a silent file.

Routes, what is selected, and every refusal with the sentence it produces: **[sites/reddit.md](sites/reddit.md)**.

## 8. SoundCloud

Page hydration, then the transcoding document it names: an MP3 copied verbatim, the only non-ISO-BMFF media this program writes. Sets, Go+ tracks and opus-only renditions are refusals by name.

Routes, what is selected, and every refusal with the sentence it produces: **[sites/soundcloud.md](sites/soundcloud.md)**.

## 9. Twitch

One batched GraphQL request for the playback token and the video's metadata, then the usher playlist assembled by the HLS fetcher. A channel is refused: a live stream has no end.

Routes, what is selected, and every refusal with the sentence it produces: **[sites/twitch.md](sites/twitch.md)**.

## 10. What a refusal looks like, and what it does not

A refusal is a **failure**: exit 1, one line on stderr, and — under `--json` —
an object with `"status": "failed"` and a `reason`. It is not a skip that exits
0, because a user who asked for one file and got none has not succeeded.

Nothing is ever written on a refusal. No empty file, no directory, no truncated
`.mp4` with a valid header: the extractors run before the first byte of media is
requested, so a refusal costs one or two page requests and no disk.

## 11. When a site changes

Every extractor is written to *fail* rather than to produce something wrong:
`read_player` returns "no formats" instead of panicking on a missing field, the
JSON lookup helpers are total, and media URLs are used only when they are
strings. A changed page therefore produces a refusal with the status the site
reported, which is the single most useful thing a bug report can contain.

## 12. The escape hatch: `--extractor-args`

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
