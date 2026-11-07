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
thumbnails, live streams, more than three sites.

## 2. v0.2 — the shapes users actually hit

| Item | Why it is next |
| --- | --- |
| **HLS demuxer** (`.m3u8` → MP4) — *done: playlists, TS demuxing, the MP4 writer, the fetch pipeline and AES-128 segments* | The single largest hole. Every X post that only offers a playlist, every TikTok livestream replay, and most of the sites on the v0.3 list speak HLS first. MPEG-TS to ISO-BMFF without re-encoding: PAT/PMT, PES, access-unit boundaries, a sample table built from PTS/DTS, and `EXT-X-MAP` init segments for the fMP4 variant. |
| **Encrypted HLS segments** (`EXT-X-KEY`) — *done: AES-128-CBC with the pure-Rust `aes`/`cbc` crates* | Falls out of the demuxer, with one correction to an earlier draft of this page: `ring` does **not** expose CBC, only AEAD modes, so AES-128-CBC decryption uses RustCrypto instead. Rust dependencies are not the thing §0 of [dependencies](dependencies.md) is about, so the rule is satisfied without an argument. `SAMPLE-AES` stays out: it hides the picture inside the container's framing, which is white-box work rather than a cipher. |
| **`--cookies FILE`** — *done: Netscape format, per-request domain/path/`Secure`/expiry matching, never written back, never logged* | Age-gated YouTube, private X posts, TikTok that refuses a session. Netscape format only, read once. Still no sign-in flow. |
| **Playlists and channels** — *`--playlist-items` is in for HLS segment lists: `1-5,9`, 1-based, nothing selected is a refusal* | The same grammar must reach a channel or a playlist page, with the per-item independence the URL list already has: one dead video must not stop the other nine. That needs a second extractor route (a playlist page is not a watch page) and its own refusal table. |
| **`--write-thumbnail`, `--write-subs`, `--sub-langs`** — *in: the poster from all three sites, YouTube captions as WebVTT with a per-language refusal table* | The remaining sidecar flags: two nearly remove the main remaining reason to reach for `yt-dlp`, and subtitles need YouTube's `timedtext` route plus a second refusal table for the languages a video does not have. Sidecar files only; muxing a subtitle track into MP4 is v0.3. (`--write-info-json` is in.) |
| **An independent demuxer in the tests** — *in: `tests/common/mp4.rs` reads a written file with its own box walker, sharing no code with `mux` or `mp4write`* | Verify a remux with a parser that shares no code with the writer (see [guarantees §6](guarantees.md#6-independence-stated-plainly)). Cheap, and it is the safety net the HLS work needs. |
| **Packaging channels** — *done on Windows: `packaging/windows/install.ps1` (per-user, checksum-verified, `-Uninstall`) and winget manifests rendered from the shipped ZIP; `.deb`/`.rpm`, Homebrew and `install.sh` shipped before it* | Still open: AUR, Alpine `.apk`, Scoop, and a flake. The binary is static; distribution is the only thing standing between it and `apk add`. |

## 3. v0.3 — reach, and the parts that are not a URL

| Item | Why |
| --- | --- |
| **More sites** | Vimeo, Reddit (`v.redd.it`), SoundCloud (audio-only), Twitch VODs. Each one needs HLS from v0.2, its own refusal table, and a fixture. A site is added only when the shapes it *cannot* reach are written down. |
| **Fragmented MP4** | DASH `SegmentBase` and live recordings arrive as `moof`/`mdat` fragments. The remuxer refuses them by name today; stitching them is the same work as HLS, applied to a second container. |
| **Subtitle tracks inside MP4** | `wvtt` (WebVTT) and `stpp` (TTML) sample entries, written by the same track builder the muxer uses. Sidecars ship first because they cannot break a video. |
| **`--concurrent-fragments N`** | Segmented sources make parallelism worth it; progressive files do not. Same flag name `yt-dlp` uses, because muscle memory is a feature. |
| **`cargo fuzz` targets** | `mux::scan`, `mux::verify` and `sites::json_after` are the parsers that read bytes from someone else. A corpus plus a nightly job, run out of band. |
| **`--extractor-args SITE:key=value`** | An escape hatch so a site change does not need a release. Deliberately ugly, deliberately undocumented beyond one table: it is a pressure valve, not an interface. |

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
