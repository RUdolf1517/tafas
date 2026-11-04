# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

What counts as a breaking change here is the *contract*, not the command line:
changing a default, an exit code's meaning or a JSON field's meaning is a major
version; adding a site, a flag or a container is minor. The policy is in
[docs/releases.md](docs/releases.md).

## [Unreleased]

## [0.1.0] - 2026-11-04

The first release. Everything below is new, and the boundaries are as much a
part of it as the features.

### Added

- **YouTube capture.** The watch page's inlined `ytInitialPlayerResponse` is
  parsed first, then InnerTube is asked through a fixed client chain
  (`android_vr`, `ios`, `android`, `tvhtml5_simply_embedded_player`, `web`). A
  client response is accepted only when it yields at least one directly playable
  format. URL shapes: `/watch?v=`, `youtu.be/`, `/shorts/`, `/embed/`, `/live/`,
  `music.youtube.com`.
- **TikTok capture.** The page's `__UNIVERSAL_DATA_FOR_REHYDRATION__` document,
  then the mobile feed endpoint when the page is walled. Short links
  (`vm.tiktok.com`, `vt.tiktok.com`) are resolved. The watermark-free
  `playAddr` is preferred over `downloadAddr`.
- **X capture.** The syndication endpoint used by embedded tweets: video,
  animated-GIF MP4 variants, and photo posts. `t.co` links are resolved.
- **A remuxer written here.** DASH video and audio are joined into one MP4
  without re-encoding: chunk offsets remapped, track ids renumbered, durations
  rescaled into the movie timescale, `moov` written first. Fragmented input and
  offsets outside an `mdat` are refused rather than guessed at.
- **Structural verification of every remux.** The written file is re-read from
  disk: both handlers must be present, every chunk offset must point inside an
  `mdat`, every track must have samples. A failure exits 4 and leaves no
  destination.
- **Atomic commit.** Every destination is written as `<target>.part`, flushed,
  synced, verified, then renamed; the parent directory is synced afterwards.
- **Resume** (`--resume`, the default). A `.part` is continued with a range
  request; a server that answers `200` instead of `206` is detected and the file
  is truncated rather than appended twice.
- **Retries** with a growing delay for transport errors, `429` and `5xx`; never
  for other `4xx`.
- **Selection controls**: `-Q/--quality best|worst|<height>`, `--audio-only`,
  `--format ID`, `--no-mux`, `--list-formats`.
- **Output controls**: `-o/--output` (file, directory, or `-` for a single
  stream on stdout), `--name`, `--overwrite`, `-n/--dry-run`.
- **A Unix interface**: the written path on stdout and everything else on
  stderr, a closed pipe answered with silence and success, exit codes `0/1/2/3/4`
  with `4` reserved for a verification failure, `--json` on stdout with a stable
  schema, `--quiet`, `--no-progress`, `--color` with `NO_COLOR`/`CLICOLOR`/
  `CLICOLOR_FORCE`/`TERM=dumb` precedence, and `-v` for request logging.
- **`--capabilities`**, a read-only JSON description of the build, and `-i/--info`.
- **A manual page and generated completions**: `man tafas`, and
  `tafas --completions bash|zsh|fish|powershell|elvish` from the live parser.

### Refused, on purpose

These are code paths with tests and a paragraph each in
[docs/extractors.md](docs/extractors.md); they are listed here so that nobody
discovers them in production.

- **Signature-protected YouTube formats.** Deciphering means executing the
  site's player JavaScript; a video whose every format is protected fails with a
  refusal that says so.
- **Sign-in checks and age gates.** No cookies are sent, accepted or stored.
- **Live streams.** Fragment stitching is not in this release.
- **HLS-only X posts.** Demuxing MPEG-TS and rebuilding sample tables is not in
  this release.
- **Fragmented MP4 input.** The remuxer copies sample tables; a fragmented file
  has none to copy.
- **Photo-less, video-less posts** and posts the syndication endpoint no longer
  knows.

### Notes

- The binary links `rustls` with the `ring` provider, so it has no OpenSSL
  dependency; `ring` is the only component in the graph that contains C and
  assembly, which [docs/dependencies.md](docs/dependencies.md) states plainly.
- The crate is `#![forbid(unsafe_code)]` and spawns no subprocess.
- `tafas` writes no configuration file, keeps no state between runs, and sends no
  telemetry.
