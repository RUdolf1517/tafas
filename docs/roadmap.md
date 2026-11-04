# Roadmap

Related: [releases](releases.md) · [extractors](extractors.md)

A version number in this document is a **plan**, not a promise. What is a
promise is §5: the list of things this project will not do.

## 1. v0.1.0 — the first release (current)

Shipped: YouTube (watch page + InnerTube client chain), TikTok (page + mobile
feed), X (syndication); DASH remuxing with verification; atomic commit; resume;
retries; `--json`; a manual page; completions; the four guarantee classes.

Not in v0.1.0, on purpose: no cookies, no signature solving, no HLS, no
playlists, no subtitles, no live streams.

## 2. v0.2 — the shapes users actually hit

| Item | Why |
| --- | --- |
| **HLS demuxer** (`.m3u8` → MP4) | X posts that offer only a playlist, and every TikTok livestream replay. MPEG-TS to ISO-BMFF without re-encoding: PAT/PMT, PES, access-unit boundaries, and a sample table built from PTS/DTS. The largest single piece of work on this list. |
| **`--cookies FILE`** | Age-gated YouTube, private X posts, TikTok that demands a session. Netscape-format only, never a browser profile, and never written to a log. |
| **Playlists / channels** | `--playlist-items 1-5`, with the same per-item independence the URL list already has. |
| **`--write-info-json`, `--write-thumbnail`** | Two flags that cost almost nothing and remove a reason to reach for `yt-dlp`. |

## 3. v0.3 — beyond the edge

| Item | Why |
| --- | --- |
| **Fragment stitching** | YouTube live and DASH `SegmentBase` variants that arrive fragmented. The remuxer refuses these today with a named reason. |
| **PO tokens** | The attestation some clients now require. It means running a token generator, which is a different program's job; the decision to depend on one belongs to the owner, not to a pull request. |
| **`--extractor-args`** | Per-site escape hatches, so a site change does not need a release. |
| **An independent demuxer in the test suite** | The honest fix for [guarantees §6](guarantees.md#6-independence-stated-plainly): verify a remux with a parser that shares no code with the writer. |

## 4. Not planned for v0.1.0, and why they are listed at all

- **A browser engine.** Solving YouTube's signature means executing JavaScript.
  A minimal JS interpreter that is *correct enough to be trusted with a
  signature* is a project of its own, and the failure mode of getting it subtly
  wrong is a corrupt download rather than an error.
- **A GUI.** Not because it would be hard, but because the CLI is the product.
- **Transcoding.** Every quality-reduction feature ("make it 480p") belongs to
  `ffmpeg`; a converter that re-encodes would break G3 and the whole argument
  that the file is what the site served.

## 5. What will not happen

| Never | Because |
| --- | --- |
| re-encoding, resolution changes, bitrate targets | G3 is the project's reason to exist |
| shipping an external binary (yt-dlp, ffmpeg, a headless browser) | the binary must work on a machine with no package manager |
| an update check or telemetry | a downloader that phones home is not a downloader |
| a config file or `TAFAS_*` variables | every decision is an argument, so a command that worked yesterday works today |
| writing a file that failed verification | exit 4 and a bug report instead |
| silently skipping something | a refusal is a failure with a name |
