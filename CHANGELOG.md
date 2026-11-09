# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

What counts as a breaking change here is the *contract*, not the command line:
changing a default, an exit code's meaning or a JSON field's meaning is a major
version; adding a site, a flag or a container is minor. The policy is in
[docs/releases.md](docs/releases.md).

## [0.2.0] - 2026-11-08

The first release since 0.1.0, and the one that carries all four planned
versions: v0.2, v0.3, v0.4 and v0.5 of the [roadmap](docs/roadmap.md). A minor
number, because everything here adds — sites, flags, containers, channels — and
nothing removes a flag or changes the meaning of an exit code or a JSON field.

Two things are in the tree and deliberately not in this archive, because both
need a key this repository does not hold: the release is **unsigned** until
`packaging/minisign.pub` exists, and an Alpine `.apk` built from these assets
installs with `--allow-untrusted`. The publish job says so rather than
publishing something nobody can check.

### Added

- **List pages: playlists, channels, albums and collections.** A YouTube
  playlist or channel (`@handle`, `/channel/`, `/c/`, `/user/`), a Vimeo album
  or showcase and a Twitch collection are read as a *list* rather than as a
  stream, and each entry is then captured on its own, into a directory, as
  `03 - Title [id].mp4` — numbered by its position on the page, so `ls` sorts
  the way the browser does. `--playlist-items` selects with the grammar it
  already had, one dead entry does not stop the others, and the run exits `1`
  if any entry failed. One level is followed: a list of lists is a refusal.
  Documented per site: [YouTube](docs/sites/youtube.md),
  [Vimeo](docs/sites/vimeo.md#3-albums-and-showcases),
  [Twitch](docs/sites/twitch.md#2-collections).
- **A build with one site, and a table instead of a `match`.**
  Every site is a row in one table (`src/sites/registry.rs`), and every row is
  a Cargo feature: `--no-default-features --features youtube` builds a binary
  that contains no code for the other six and refuses their URLs by saying
  which sites it does have. All seven are on by default, and a `features` job
  measures both binaries so the saving is a number rather than a claim.
  Documented in [docs/extractors.md §2](docs/extractors.md#2-the-reader-table-and-a-build-with-one-site).
- **One documentation page per site.** `docs/extractors.md` keeps what applies
  to every site — detection, refusals, the escape hatch — and each site's
  routes and refusal table is a page of its own under `docs/sites/`.
- **An Alpine `.apk`, and AUR, Scoop and flake files.** The `.apk` is built
  from the same static tarball as the `.deb` and `.rpm` by `packaging/apk.py`,
  in the v2 format that every apk-tools since 2.0 reads, and is unsigned, so
  installing it asks for `--allow-untrusted`. The `PKGBUILD`,
  the Scoop manifest and a `flake.nix` are rendered at release time from
  `SHA256SUMS` by `packaging/channels.py`, so every hash in them is the hash of
  an archive that exists; `--self-test` renders all three from a stand-in
  release on every pull request.
- **Reproducible releases, a signature and an SBOM.** The release build no longer
  carries its own clock, directory or cargo home: `SOURCE_DATE_EPOCH` is the
  commit's time, `--remap-path-prefix` folds the runner's paths into `/build` and
  `/cargo`, and `CARGO_INCREMENTAL` is off. The release publishes
  `tafas-release-X.Y.Z.spdx.json`, an SPDX 2.3 document built from `Cargo.lock` by
  `packaging/sbom.py` — 154 packages, sorted, reproducible, no network. When the
  `MINISIGN_SECRET_KEY` secret exists, `packaging/sign.sh` signs every artifact
  and `SHA256SUMS`, publishes `minisign.pub` beside them and verifies all of it
  against that key before uploading; without a published key it refuses to sign
  rather than publish a signature nobody can check. Documented in
  [docs/releases.md](docs/releases.md).
- **An `msrv` job.** `rust-version = "1.82"` was a line in the release procedure
  and is now a job: `cargo build --locked --all-targets` at the declared floor,
  with the failure published to the pull request. A dependency that raises the
  floor fails here instead of in a release.
- **Windows long paths.** `paths::verbatim` puts `\\?\` (or `\\?\UNC\`) in
  front of the paths tafas writes — the `.part`, the rename into place, a deep
  output directory, the HLS cache sidecars, and the three sidecars written beside
  the media (`--write-info-json`, `--write-subs`, `--write-thumbnail`), which
  inherit the media's depth and were the ones that failed at 260 characters. A
  path where prefixing could change the meaning (relative, already verbatim,
  `.`/`..`, forward slashes) is left exactly as it was.
- **`--json` writes exactly one object per item.** A failure used to be printed
  by the code that recorded it *and* by the code that announced the item, so one
  broken URL produced two identical lines and `--list-formats` produced a table
  followed by an object. One item is one object now, whether it failed or not,
  and whether the run was serial or `-j`.
- **A progress line that fits.** The bar is drawn as wide as the terminal says
  (`COLUMNS`) and gives up its width before the numbers do; below 24 columns it
  is left alone rather than guessed at.

- **Per-segment resume.** An interrupted HLS download no longer starts over. The
  segments that landed are cached beside the staging file
  (`<target>.part.segments`, one length per line in `<target>.part.journal`,
  separately for a split audio rendition), and a rerun replays them and asks the
  server only for what is missing — and says how many it replayed. The journal
  starts with a fingerprint of the selection, so another playlist, another
  `--playlist-items` or another build drops the cache instead of splicing it into
  a file it does not describe; the payload is written and synced before its
  length is recorded, so a torn tail is cut back to the last whole segment.
  `--no-resume` deletes the cache before the first request. Documented in
  [docs/hls.md](docs/hls.md#6-what-is-resumed).

- **`--limit-rate RATE`.** One budget for the bytes coming off the wire: a
  progressive body and every segment of an HLS list are charged against the same
  clock, so `--concurrent-fragments 4` divides the rate rather than multiplying
  it. `RATE` takes an optional `K`, `M` or `G` suffix in powers of 1024 with
  decimals allowed; below `1K` the run would be slower than its own timeouts so
  that is a usage error (2) rather than a clamp, and the ceiling is `1G`.
  Metadata — the page, the JSON, a thumbnail — is not charged. The wait happens
  outside the budget's lock, so concurrent segments still overlap, and one
  chunk of slack at the start keeps the first read instant. Documented in
  [docs/cli.md](docs/cli.md#4-transfer).
- **Fragmented MP4 in HLS** (`src/mp4read.rs`). A playlist with `EXT-X-MAP` is
  no longer refused: the header is fetched once, `moov`/`trak`/`stsd` are read for
  the timescale and the codec record (`avcC`, or `esds` down to the two-byte
  `AudioSpecificConfig`), and each segment's `moof`/`trun`/`tfhd`/`tfdt` becomes
  the same `Track`/`Sample` pair the transport-stream demuxer produces, so
  `mp4write` lays both out unchanged. Defaults cascade `trun` → `tfhd` → `trex`,
  decode times come from `tfdt` and carry across fragments, and every sample
  offset is bounds-checked against the bytes it came in. `SAMPLE-AES` (`senc`),
  HEVC and AC-3 sample entries, a swapped `EXT-X-MAP` and a header that is not
  ISO-BMFF are refused by name. Documented in [docs/hls.md](docs/hls.md).
- **`--extractor-args SITE:KEY=VALUE`.** A closed, checked escape hatch for the
  day a site reorders the document an extractor reads: `youtube:client=NAME`
  pins one InnerTube client instead of walking the chain, and
  `tiktok:route=web|app` drops one of TikTok's two routes. The vocabulary is
  validated before the first request — an unknown key or value is exit 2 with
  the accepted spellings in the message — a pinned route is named in the
  refusal it causes, and `--capabilities` publishes the same lists under
  `extractor_args`. Nothing can be enabled that a release did not ship.
- **`--concurrent-fragments N`.** Fetch up to sixteen segments of one HLS list
  at once instead of one after another; the default of `1` is the behaviour that
  existed before. Arrivals are written into their own slot, so the file is the
  playlist's order whatever the network does; the progress counter moves as
  segments land. A failure cancels the segments still queued rather than
  fetching bytes for a file that will not exist, and the flag is bounded to
  `1`–`16` before the first request. Like `--playlist-items` it applies to a
  segment list only, and says so when it does not.
- **A per-user Windows installer** (`packaging/windows/install.ps1`, published as
  a release asset and covered by `SHA256SUMS`). Installs into
  `%LOCALAPPDATA%\Programs\tafas`, adds one user `PATH` entry, and refuses to
  write anything it has not verified: the archive is checked against the release's
  `SHA256SUMS`, must contain exactly `tafas.exe` and `LICENSE`, and the extracted
  binary must report the version the tag names. Replacement is a directory rename
  with a backup, so a failure puts the previous version back; a directory the
  installer does not own is never adopted or deleted, and `-Uninstall` removes only
  the files its own marker lists. No elevation, no MSI, no MSIX, nothing
  machine-wide.
- **Local winget manifests generated from the shipped ZIP**
  (`packaging/winget/`). The release job renders the three manifests from the exact
  bytes of `tafas-x86_64-pc-windows-msvc.zip`, so `InstallerSha256` is the hash of
  the asset rather than of a rebuild, and attaches them as a bundle for
  `winget install --manifest`. Nothing is submitted to `winget-pkgs`.
- **`windows-installer.yml`**: the installer's integration test (install, upgrade,
  checksum and version refusals, rollback, uninstall, `PATH` ownership) on
  `windows-latest`, and the manifest generator's tests on Ubuntu. Both run offline
  against fixtures built in the job.
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
- **`--cookies FILE`.** A Netscape cookie jar is read once at startup and its
  cookies are sent only where they apply: host, path, `Secure` and expiry are
  checked per request, so a jar full of browser cookies does not leak a session
  cookie to a CDN that merely shares a parent domain. Nothing is written back,
  and no cookie value is logged, printed or quoted in an error. Age-gated
  YouTube and protected X posts become reachable; there is still no sign-in
  flow and no challenge is solved.
- **`--write-info-json`.** A `<name>.info.json` beside each file: page, title,
  uploader, duration, every stream the page offered, the choice that was made
  and the files that were written. No media URLs — a signed URL is a credential
  that expires in hours — and the document is written through a temporary and a
  rename, after the media file has landed, never before.
- **`--write-subs` and `--sub-langs`.** One WebVTT file per caption track,
  `<name>.<language>.vtt`, or `<name>.<language>.auto.vtt` when the site
  transcribed the audio rather than a person writing the cues. The tracks come
  from YouTube's `captions.playerCaptionsTracklistRenderer`, and each `baseUrl`
  is asked for `fmt=vtt` at parse time. `--sub-langs` takes `all`, an exact
  code, or a `*` prefix, and a selection that matches no track is a refusal
  naming the languages the page does have. A caption body that is empty or is
  not WebVTT is a refusal too: `timedtext` answers `200` with nothing to some
  networks, and calling that "no subtitles" would be a quiet lie.
- **`--write-thumbnail`.** The page's poster frame beside each file, as
  `<name>.jpg`, `.webp` or `.png` — the site's own bytes, never resized or
  converted, with the extension following the bytes. The sources are
  `videoDetails.thumbnail.thumbnails` on YouTube (widest wins), `originCover`
  then `cover` on TikTok, and `media_url_https` on an X video. Written after the
  media file, through a temporary and a rename.
- **`--playlist-items ITEMS`.** `3`, `1-5`, `2-`, or any comma-separated mix,
  counted from one and parsed before the first request. On an HLS playlist the
  item is a segment: the file is assembled from the segments named and no
  others. A selection that matches nothing is a refusal, and a selection that
  does not begin on a keyframe is written with a warning, because its first
  frames cannot be decoded.
- **`X` posts that offer only a playlist** now download. A progressive MP4 is
  still preferred when the post offers both, and `--list-formats` shows the
  playlist as `hls`.
- **Vimeo.** `vimeo.com/<id>`, channel URLs, unlisted `<id>/<hash>` URLs and
  `player.vimeo.com/video/<id>`, read from the player's own configuration
  endpoint — one request, no HTML. Progressive MP4s are offered as the video
  files they are, the HLS rendition is kept beside them for videos that have only
  that, and the poster and duration come from the same document. DASH is not
  read and the refusal names `SegmentBase`; a video behind a password is a
  refusal that says so; an unlisted video without its hash gets the same `404` a
  deleted one gets. Shapes this cannot reach are in
  [sites/vimeo](docs/sites/vimeo.md).
- **Reddit.** `/r/<sub>/comments/<id>/`, `redd.it` and bare `v.redd.it/<hash>`
  URLs. A post is read from `/comments/<id>.json`; a bare media URL needs no
  JSON, because its playlist lives at a fixed path. One route is taken —
  `hls_url`, a master playlist with an `EXT-X-MEDIA` audio rendition that
  `hlsfetch` joins into one MP4 — and the two obvious alternatives are refusals
  that say why: `fallback_url` (`DASH_<height>.mp4`) is a **video-only** MP4,
  and `dash_url` is `SegmentBase`, which this build does not read. A removed
  post, a gallery or a text post and an over-18 post with nothing playable are
  each named. The limitations are in
  [sites/reddit](docs/sites/reddit.md).
- **SoundCloud.** Track URLs and `on.soundcloud.com` short links, read from the
  page's own hydration document, then the transcoding it names, then the bytes.
  The result is an **MP3** — the first non-ISO-BMFF media this program writes, and
  the reason `Container::Mp3` exists: there is no container to rebuild and nothing
  beyond the byte count to verify, which `docs/cli.md` §3 and
  [sites/soundcloud](docs/sites/soundcloud.md) both say out loud. A set, a
  Go+ track, an opus-only rendition and a consent wall are refusals by name; the
  public `client_id` is scraped from the page rather than hard-coded.
- **Twitch VODs.** `twitch.tv/videos/<id>`, through one batched GraphQL request:
  `PlaybackAccessToken` (asked by persisted-query hash, with the public web
  client id — not a credential, and `SECURITY.md` now says so beside YouTube's
  key) and a `video(id:)` query for the title, the uploader, the length and the
  poster. The token is a JSON document, so it is percent-encoded into
  `nauth`/`nauthsig` on the usher playlist, which `hlsfetch` assembles like any
  other list. A channel (a live stream has no end), a clip, a subscriber-only
  recording and an API error are refusals that say which they are;
  [sites/twitch](docs/sites/twitch.md).
- **`Url::join`**, with dot-segment removal: playlist URIs are relative far more
  often than they are absolute, and `../` is common.
- **`--embed-subs`.** The cues of one caption track go *inside* the MP4 as a
  `wvtt` track (WebVTT in ISO-BMFF): the file the run just wrote is read back
  with the sample-table reader, the cues become one more `trak` — `hdlr` `subt`,
  `sthd`, a `wvtt` sample entry with an empty `vttC` — and the whole thing is laid
  out again, samples passed through byte for byte. Timescale 1000, one sample per
  cue, a `vttc`→`payl` payload with the text, each sample stating its own
  duration, and a gap between two cues turned into a positive `ctts` offset — a
  decode time cannot be stated in `stts`, only a length, so the cue's own time
  rides in the composition offset and it still appears when the body said. Cue settings, `STYLE` and `REGION`
  blocks are dropped, and the track's `mdhd` language is `und`: both are in
  `docs/cli.md` §3. One track per file, so the selection has to name one
  language; a run that did not produce exactly one MP4 says so and changes
  nothing. `stpp`/TTML is refused by name rather than converted.
- **The sample-table MP4 reader** (`mp4read::plain`): a progressive file's
  `stbl` walked into tracks and samples — `stsz`, `stts`, `ctts` (v0 and v1),
  `stss`, `stco`/`co64` and `stsc` — with every sample bounds-checked against the
  file and a fragmented file refused by naming the `moof`. It is what
  `--embed-subs` reads the file back with, and it is verified against a file the
  writer made.

### Changed

- **`FALLBACK_API_KEY` is now `PUBLIC_WEB_API_KEY`, and the comment says what it
  is.** The string is YouTube's own web-client key, public and in every watch
  page; secret scanning flags it because it is shaped like a Google API key, and
  there is nothing to rotate. [SECURITY.md](SECURITY.md) now says that out loud,
  explains why the fallback exists, and says that no other alert gets the same
  treatment.

`--quality` now applies to a variant list as well: a height ceiling picks the
best variant at or below it, and `best`/`worst` fall back to bandwidth when the
playlist states no resolution. Both HLS containers are read, and the independent
demuxer in `tests/common/mp4.rs` checks what is written. Captions can now live in
the file as well as beside it, and a progressive MP4 can be read as well as
written. Next are the fuzz targets and the remaining v0.3 sites. Nothing in this
section removes a flag.

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
