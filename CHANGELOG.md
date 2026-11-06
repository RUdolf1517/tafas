# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

What counts as a breaking change here is the *contract*, not the command line:
changing a default, an exit code's meaning or a JSON field's meaning is a major
version; adding a site, a flag or a container is minor. The policy is in
[docs/releases.md](docs/releases.md).

## [Unreleased]

### Added

- **HLS playlist parsing** (`src/hls.rs`). Master and media playlists, variant
  selection by height and bandwidth, `EXT-X-MAP` initialisation sections,
  `EXT-X-BYTERANGE` in both the absolute and the continue-from-the-last form,
  `EXT-X-KEY` with AES-128 keys (including the implicit IV that most playlists
  omit), and `EXT-X-MEDIA` audio renditions. A playlist without `EXT-X-ENDLIST`
  is refused with a reason: there is no finished recording to write.
- **MPEG-TS demuxing** (`src/ts.rs`). PAT and PMT, PES reassembly per PID,
  Annex B to AVCC with the SPS and PPS hoisted into an `avcC` record, ADTS
  frames split out with the `AudioSpecificConfig` an `esds` box needs. HEVC,
  MPEG audio and AC-3 are refused by name rather than written with a guessed
  codec record.
- **An MP4 writer for those tracks** (`src/mp4write.rs`). `ftyp`, `moov` first,
  one `mdat`: `avc1`+`avcC`, `mp4a`+`esds`, `stts` from decode times, `ctts` only
  when presentation and decode times differ, `stss` for keyframes. The finished
  file is read back by the remuxer's own verifier before the caller hears about
  it.
- **The HLS fetch pipeline** (`src/hlsfetch.rs`, `Http::get_bytes`). A master
  playlist is walked to one variant; that variant's segment list is fetched one
  segment at a time, with `EXT-X-BYTERANGE` asked for as a range request; the
  concatenated transport stream is demuxed and written as an MP4. A variant with
  an audio rendition gets that rendition as a second track — unless its own
  segments already carry audio, which would otherwise be written twice. A
  server that answers a range request with the whole object is refused when the
  window does not start at zero, and the window is cut on this side when it
  does.
- **AES-128 HLS segments** (`aes`, `cbc`). The key is fetched once per URI and
  every segment is decrypted in memory with the IV the playlist states or the
  media sequence it implies. A segment that does not decrypt — wrong key, wrong
  IV, truncated body — is a refusal, because the padding check is the only
  integrity signal there is. `SAMPLE-AES` is still refused by name.
- **`X` posts that offer only a playlist** now download. A progressive MP4 is
  still preferred when the post offers both, and `--list-formats` shows the
  playlist as `hls`.
- **`Url::join`**, with dot-segment removal: playlist URIs are relative far more
  often than they are absolute, and `../` is common.

`--quality` now applies to a variant list as well: a height ceiling picks the
best variant at or below it, and `best`/`worst` fall back to bandwidth when the
playlist states no resolution. One gate remains, named before any file is
created: a playlist of fMP4 segments (`EXT-X-MAP`). Next are `--cookies`,
`--playlist-items` and the `--write-*` family. Nothing in this section changes
any existing flag.

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
