# Roadmap

Related: [releases](releases.md) · [extractors](extractors.md) ·
[dependencies](dependencies.md) · [guarantees](guarantees.md)

A version number here is a **plan**, not a promise. What is a promise is §7: the
list of things this project will not do. Everything above it is ordered by what
breaks a user's afternoon, not by what is interesting to write.

Two rules keep this list honest:

- an item moves up only when someone can name the URL that fails without it;
- an item that stays unsupported gets a refusal text in
  [extractors](extractors.md), because "not yet" is a feature only if it says so.

## 1. v0.1.0 — shipped

YouTube (watch page + InnerTube client chain), TikTok (page rehydration + mobile
feed), X (syndication). DASH remuxing without re-encoding, verified by a second
reader before the file is renamed. Atomic `.part` commit, ranged resume, retries,
`--json`, a manual page, completions, `--capabilities`.

Deliberately absent: cookies, signature solving, HLS, playlists, subtitles,
thumbnails, live streams, more than four sites.

## 2. v0.2 — the shapes users actually hit

| Item | Why it is next |
| --- | --- |
| **HLS demuxer** (`.m3u8` → MP4) — *done: playlists, TS demuxing, the MP4 writer, the fetch pipeline, AES-128 segments and the fMP4 variant* | The single largest hole. Every X post that only offers a playlist, every TikTok livestream replay, and most of the sites on the v0.3 list speak HLS first. MPEG-TS to ISO-BMFF without re-encoding: PAT/PMT, PES, access-unit boundaries, a sample table built from PTS/DTS, and `EXT-X-MAP` init segments for the fMP4 variant. |
| **Encrypted HLS segments** (`EXT-X-KEY`) — *done: AES-128-CBC with the pure-Rust `aes`/`cbc` crates* | Falls out of the demuxer, with one correction to an earlier draft of this page: `ring` does **not** expose CBC, only AEAD modes, so AES-128-CBC decryption uses RustCrypto instead. Rust dependencies are not the thing §0 of [dependencies](dependencies.md) is about, so the rule is satisfied without an argument. `SAMPLE-AES` stays out: it hides the picture inside the container's framing, which is white-box work rather than a cipher. |
| **`--cookies FILE`** — *done: Netscape format, per-request domain/path/`Secure`/expiry matching, never written back, never logged* | Age-gated YouTube, private X posts, TikTok that refuses a session. Netscape format only, read once. Still no sign-in flow. |
| **Playlists and channels** — *`--playlist-items` is in for HLS segment lists: `1-5,9`, 1-based, nothing selected is a refusal* | The same grammar must reach a channel or a playlist page, with the per-item independence the URL list already has: one dead video must not stop the other nine. That needs a second extractor route (a playlist page is not a watch page) and its own refusal table. |
| **`--write-thumbnail`, `--write-subs`, `--sub-langs`** — *in: the poster from all three sites, YouTube captions as WebVTT with a per-language refusal table* | The remaining sidecar flags: two nearly remove the main remaining reason to reach for `yt-dlp`, and subtitles need YouTube's `timedtext` route plus a second refusal table for the languages a video does not have. Sidecar files only; muxing a subtitle track into MP4 is v0.3. (`--write-info-json` is in.) |
| **An independent demuxer in the tests** — *in: `tests/common/mp4.rs` reads a written file with its own box walker, sharing no code with `mux` or `mp4write`* | Verify a remux with a parser that shares no code with the writer (see [guarantees §6](guarantees.md#6-independence-stated-plainly)). Cheap, and it is the safety net the HLS work needs. |
| **Packaging channels** — *done on Windows: `packaging/windows/install.ps1` (per-user, checksum-verified, `-Uninstall`) and winget manifests rendered from the shipped ZIP; `.deb`/`.rpm`, Homebrew and `install.sh` shipped before it* | Still open: AUR, Alpine `.apk`, Scoop, and a flake. The binary is static; distribution is the only thing standing between it and `apk add`. |

## 3. v0.3 — reach, and the parts that are not a URL

| Item | Why |
| --- | --- |
| **More sites** — *done: Vimeo, Reddit, SoundCloud (MP3), Twitch VODs* | Vimeo: the player config, progressive MP4 plus HLS, DASH and password-protected videos refused by name. Reddit: `/comments/<id>.json` and one route only — `hls_url` — because `fallback_url` is a video-only MP4 and `dash_url` is `SegmentBase`. SoundCloud: page hydration, then the transcoding, then the bytes, as an MP3 copied verbatim. Twitch: one batched GraphQL request for the playback token and the metadata, then the usher playlist. What is still out is named per site in [extractors](extractors.md): clips, live channels, Go+ tracks, subscriber-only recordings. |
| **Fragmented MP4** — *done: `EXT-X-MAP` headers, `moof`/`trun`/`tfhd`/`tfdt` parsing into the writer's own tracks, HEVC and `SAMPLE-AES` refused by name* | DASH `SegmentBase` and live recordings arrive as `moof`/`mdat` fragments. The HLS flavour is read ([hls §2.1](hls.md#21-fragmented-mp4)); a progressive file that is *itself* fragmented is refused by name by the sample-table reader, which is the same answer the remuxer gave and now says which `moof` it found. |
| **A progressive MP4 read back** — *done: `mp4read::plain`, `stsz`/`stts`/`ctts`/`stss`/`stco`/`co64`/`stsc` walked into tracks, every sample bounds-checked* | Reading what the writer wrote is the only way to add to a finished file, and it is what verifies a round trip. It does not replace `mux::verify`: one answers "is this file sane", the other "what is in it". |
| **Subtitle tracks inside MP4** — *done: `--embed-subs`, one `wvtt` track (`subt`/`sthd`/`wvtt`/`vttC`, a `vttc`→`payl` per cue, timescale 1000, each sample stating its duration); `stpp`/TTML refused by name* | Sidecars shipped first because they cannot break a video; the track now goes in through the same builder the muxer uses, on a file that is read back first. `stpp` is the branch that was not taken: TTML has no cue text the container can hold without a second parser, so it is a refusal rather than a conversion. |
| **`--concurrent-fragments N`** — *done: 1–16 workers over one segment list, written in playlist order, first failure cancels the queue* | Segmented sources make parallelism worth it; progressive files do not. Same flag name `yt-dlp` uses, because muscle memory is a feature. |
| **`cargo fuzz` targets** — *done: `mp4read::plain`, `mp4read::dimensions`, `subtitles::cues` and `sites::json_after`, in a `fuzz/` package the binary never links, run weekly by `fuzz.yml` and never on a pull request* | The parsers that read bytes from someone else are the ones that deserve a corpus; `mux::verify`/`remux` came last because they take a path — their target owns a temporary file, which makes it the slow one. |
| **`--extractor-args SITE:key=value`** — *done: a closed vocabulary (`youtube:client`, `tiktok:route`), checked before the first request and published by `--capabilities`* | An escape hatch so a site change does not need a release. Deliberately ugly, deliberately documented as one table in [extractors §11](extractors.md#11-the-escape-hatch---extractor-args): it is a pressure valve, not an interface. It only narrows — every value is a route the build already tries. |

## 4. v0.4 — speed, platforms, trust

| Item | Why |
| --- | --- |
| **`--limit-rate`, per-fragment resume** | A downloader that saturates a shared line is a downloader people stop using. |
| **Reproducible builds and signed releases** | `SOURCE_DATE_EPOCH`, `--remap-path-prefix`, a minisign key next to `SHA256SUMS`, and an SBOM in the archive. The checksum proves the file matches the release; a signature proves the release is ours. |
| **Budgets in CI** | Binary size and `--version` startup time, asserted with a tolerance. A downloader is allowed to grow; it is not allowed to grow silently. |
| **MSRV job** | Once the dependency floor stops moving, a `1.82` build joins the matrix instead of being a line in the release procedure. |
| **Windows polish** | Long paths (`\\?\`), progress width on narrow consoles, and a `.zip` that keeps the manual page. |

## 5. v0.5 — extensibility without a fork

| Item | Why |
| --- | --- |
| **A stable `Extractor` trait** | The three readers already share a shape: `probe(url)`, `extract(page)`, `refusals()`. Publishing it (with a semver promise) lets someone add a site without owning `main`. |
| **Feature-per-site builds** | `--no-default-features --features youtube` gives a binary that cannot touch the other two sites: smaller, and auditable by a user who only wants one. This is tzim's model, and it has aged well. |
| **Per-site documentation pages** | The refusal tables in [extractors](extractors.md) are already a table; splitting them into one page per site keeps that page readable past five sites. |

## 6. Ideas that are parked, not planned

- **A JavaScript engine for signature ciphers.** A minimal interpreter that is
  *correct enough to be trusted with a signature* is a project of its own, and
  the failure mode of getting it subtly wrong is a corrupt download rather than
  an error. Parked until someone wants to own it.
- **PO tokens / attestation.** Running a token generator means shipping a second
  program's logic. That decision belongs to the owner, not to a pull request.
- **A GUI, a daemon, a library of every site.** Each would make the CLI worse at
  the one thing it does.

## 7. What will not happen

| Never | Because |
| --- | --- |
| re-encoding, resolution changes, bitrate targets | the file is what the site served, or it is not this tool's output |
| shipping an external binary (yt-dlp, ffmpeg, a headless browser) | the binary must work on a machine with no package manager |
| a dependency in a language other than Rust unless it can be neither written here nor replaced by a Rust crate | [dependencies §0](dependencies.md#0-the-rule); `ring` is the one exception, and it is argued, not inherited |
| an update check or telemetry | a downloader that phones home is not a downloader |
| a config file or `TAFAS_*` variables | every decision is an argument, so a command that worked yesterday works today |
| writing a file that failed verification | exit 4 and a bug report instead |
| silently skipping something | a refusal is a failure with a name |

## 8. How an item leaves this list

An item ends in one of three ways, and the third is not a failure:

1. **shipped** — it leaves the table and appears in `CHANGELOG.md`;
2. **moved** — it gets a reason in the next release's notes;
3. **dropped** — it moves to §7 with the reason it will not be done, so that the
   next person who wants it finds the argument instead of starting it again.
