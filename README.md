<div align="center">

# tafas

**תפס** — *tafas*: to catch, to seize, to hold on to

**Catch the video, keep the bytes.**

[![ci](https://github.com/RUdolf1517/tafas/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/RUdolf1517/tafas/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/RUdolf1517/tafas?style=flat-square&color=0b3d91)](https://github.com/RUdolf1517/tafas/releases/latest)
[![rust](https://img.shields.io/badge/rust-1.82%2B-dea584?style=flat-square)](https://www.rust-lang.org/)
[![license](https://img.shields.io/badge/license-MIT-0b3d91?style=flat-square)](LICENSE)

[Install](#install) · [Usage](#usage) · [Sites](#sites) · [Why tafas](#why-tafas) · [Roadmap](#roadmap) · [Docs](#documentation)

</div>

---

`tafas` takes a page URL and writes one playable file. When a site hands out
picture and sound as two separate streams — which is what YouTube does above
720p — tafas joins them itself: the sample tables are copied, the container
index is rebuilt, and nothing is re-encoded. No `ffmpeg`, no `yt-dlp`, no
`Python`, no shelling out to anything. One static binary that asks the site for
the bytes and writes them down.

What it will not do is pretend. A YouTube format that arrives
signature-protected is **dropped**, not guessed at, because solving that means
executing the site's own minified JavaScript. A page behind a sign-in check is a
refusal with a reason, not an empty file and not a hang. A post that offers only
an HLS playlist is walked segment by segment and written as an MP4 in the same
process. Everything that is out of reach is in
[docs/extractors.md](docs/extractors.md), written down before you hit it.

```console
$ tafas https://www.youtube.com/watch?v=aqz-KE-bpKQ -o ~/Videos
youtube aqz-KE-bpKQ
  ▕████████████████████░░▏  91.3%  118.4 MiB/129.7 MiB  eta 00:03
muxing 118.4 MiB + 4.1 MiB (no re-encoding)
/home/rudolf/Videos/Big Buck Bunny [aqz-KE-bpKQ].mp4
```

stdout is that last line and nothing else. Progress, notes and errors go to
stderr, so the file can be piped somewhere without a `--quiet` argument:

```console
$ tafas https://x.com/nasa/status/1732902605501431916 | xargs -r mpv
```

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/RUdolf1517/tafas/main/install.sh | sh   # binary, no Rust
brew tap RUdolf1517/tafas https://github.com/RUdolf1517/tafas && brew install RUdolf1517/tafas/tafas
cargo install --git https://github.com/RUdolf1517/tafas --tag v0.1.0                 # Rust 1.82+
```

Linux (x86_64 and ARM64), macOS (Apple silicon and Intel), Windows x64. The
install script checks the archive against the release's `SHA256SUMS` and refuses
anything that does not match. Each release also publishes an SPDX SBOM
(`tafas-release-X.Y.Z.spdx.json`) and, when the signing key is set, a minisign
signature beside every file — `minisign -Vm <file> -p minisign.pub -x
<file>.minisig`. The build itself is reproducible: `SOURCE_DATE_EPOCH` is the
commit's time and the compiler sees no directory from the machine that built it,
so two builds of one commit are one file. See
[docs/releases.md](docs/releases.md#reproducibility-signatures-and-the-sbom).

<details>
<summary>Debian, Fedora, Windows</summary>

```bash
# Debian / Ubuntu — x86_64
curl -fsSL -O https://github.com/RUdolf1517/tafas/releases/latest/download/tafas_amd64.deb
sudo apt install ./tafas_amd64.deb

# Debian / Ubuntu — ARM64
curl -fsSL -O https://github.com/RUdolf1517/tafas/releases/latest/download/tafas_arm64.deb
sudo apt install ./tafas_arm64.deb

# Fedora / RHEL — x86_64
curl -fsSL -O https://github.com/RUdolf1517/tafas/releases/latest/download/tafas.x86_64.rpm
sudo dnf install ./tafas.x86_64.rpm

# Fedora / RHEL — ARM64
curl -fsSL -O https://github.com/RUdolf1517/tafas/releases/latest/download/tafas.aarch64.rpm
sudo dnf install ./tafas.aarch64.rpm
```

Both packages install `man tafas` and completion for bash, zsh and fish.

Windows x64 — either unpack `tafas-x86_64-pc-windows-msvc.zip` from
[Releases](https://github.com/RUdolf1517/tafas/releases/latest) and put `tafas.exe`
on `PATH`, or let the per-user installer do that part:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1              # install
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall   # undo it
```

It installs into `%LOCALAPPDATA%\Programs\tafas`, adds one user `PATH` entry,
checks the archive against `SHA256SUMS` before unpacking it, and never asks for
elevation. No MSI, no MSIX, nothing machine-wide, and no registry key beyond the
user `PATH` that `-Uninstall` takes back. Each release also carries a local winget
manifest bundle: `winget install --manifest <unpacked bundle>`.

</details>

The binary is statically linked, contains no C code of its own, needs no
runtime, writes no configuration file and sends no telemetry. It has no
self-update mechanism: `tafas --version` prints a version and stops there.

## Usage

```bash
tafas https://youtu.be/aqz-KE-bpKQ                  # one URL
tafas URL1 URL2 URL3 -o ./videos/                   # several, in parallel with -j
tafas https://youtu.be/aqz-KE-bpKQ -Q 720p          # a ceiling, not an exact match
tafas https://youtu.be/aqz-KE-bpKQ --audio-only     # m4a, no video
tafas https://youtu.be/aqz-KE-bpKQ --list-formats   # what the page offers, then stop
tafas https://youtu.be/aqz-KE-bpKQ -n               # plan only, nothing fetched
tafas https://youtu.be/aqz-KE-bpKQ --json           # one object per item
tafas https://youtu.be/aqz-KE-bpKQ -o - | mpv -     # bytes on stdout
tafas https://x.com/user/status/123 --cookies ~/cookies.txt  # a session, if you have one
tafas https://youtu.be/aqz-KE-bpKQ --write-info-json # a sidecar describing the choice
tafas https://x.com/user/status/123 --playlist-items 1-5  # part of an HLS list
tafas https://x.com/user/status/123 --concurrent-fragments 4  # four segments at once
tafas https://vimeo.com/76979871 --limit-rate 2M  # leave the line usable for others
tafas https://youtu.be/dQw4w9WgXcQ --extractor-args youtube:client=ios  # one client only
tafas https://youtu.be/aqz-KE-bpKQ --write-thumbnail  # the poster, beside the file
tafas https://youtu.be/aqz-KE-bpKQ --write-subs --sub-langs en,ru  # WebVTT captions
tafas https://youtu.be/aqz-KE-bpKQ --embed-subs --sub-langs en  # the same cues, inside the MP4
```

| Exit | Meaning |
| --- | --- |
| `0` | Everything succeeded, or the answer was informational |
| `1` | At least one URL failed; the others were still captured |
| `2` | Invalid usage; nothing was requested |
| `3` | Fatal error; the run could not be completed |
| `4` | **Verification failure** — a written file broke its contract. That is a bug |
| `130` | Interrupted. A `.part` file may remain, and `--resume` continues it |

Full flag reference: [docs/cli.md](docs/cli.md). Manual page: `man tafas`.

### What `-Q` actually means

`best` is the highest resolution, then the highest bitrate, then the codec more
players accept without a plugin. A height is a **ceiling**: `-Q 720p` never
hands you 1080p, and if a video has nothing that small the smallest available
stream is taken and the substitution is reported. `-Q worst` is the smallest
stream, which is what a phone on a train wants.

## Sites

| Site | URL shapes | Route | What it writes |
| --- | --- | --- | --- |
| **YouTube** | `watch?v=`, `youtu.be/`, `/shorts/`, `/embed/`, `/live/`, `music.youtube.com` | the watch page's inlined player response, then InnerTube with a fixed client chain (`android_vr` → `ios` → `android` → embedded TV → `web`) | MP4; separate DASH streams are remuxed into one file |
| **TikTok** | `/@user/video/`, `/@user/photo/`, `vm.tiktok.com`, `vt.tiktok.com` | the page's rehydration document, then the mobile feed | MP4, watermark-free version preferred; photo posts give images |
| **X** | `/<user>/status/`, `i/web/status/`, `t.co` | the syndication endpoint the embedded widget uses | MP4 (video, or an animated GIF's MP4), the post's photos as JPEG, or — when a post offers nothing else — its HLS playlist assembled into an MP4 |
| **Vimeo** | `vimeo.com/<id>`, channel URLs, unlisted `<id>/<hash>`, `player.vimeo.com/video/<id>` | the player's own configuration endpoint; one request, no HTML | MP4 from the progressive files; a video that offers only an HLS playlist is assembled into an MP4; the poster and duration come from the same document |
| **Reddit** | `/r/<sub>/comments/<id>/`, `redd.it`, `v.redd.it/<hash>` | the post's own JSON (`/comments/<id>.json`), or the fixed playlist path for a bare media URL | MP4 assembled from the HLS playlist; the progressive `DASH_<height>.mp4` files carry no audio and are refused by name |
| **SoundCloud** | `<artist>/<track>`, `on.soundcloud.com` | the page's hydration document, then the transcoding document it names | **MP3**, copied byte for byte — sound only, no container rebuilt; sets, Go+ tracks and opus-only renditions are refused by name |
| **Twitch** | `/videos/<id>` | one batched GraphQL request: the playback token, plus the title, length and poster | MP4 assembled from the usher playlist; a live channel, a clip and a subscriber-only recording are refused by name |

A site is listed only because its reader exists and its refusals are written
down. What each one can and cannot reach — age gates, PO tokens, signature
ciphers, HEVC fragments — is one table away in
[docs/extractors.md](docs/extractors.md).

## Why tafas

| | |
| --- | --- |
| **It never re-encodes** | Picture and sound are copied as sample tables; the muxer rewrites chunk offsets, track ids and durations in `moov`, and leaves `avcC`, `av1C`, `vpcC` and `esds` byte for byte. Quality is whatever the site served. |
| **It joins streams itself** | YouTube's 1080p is two files. `tafas` builds the single MP4 in Rust — there is no `ffmpeg` to install, and no subprocess to fail in a way that looks like a network error. |
| **It refuses instead of guessing** | Cipher-protected formats, `SAMPLE-AES` segments, HEVC fragments, age-gated pages and posts that need an account each exit with a named reason and a link to the paragraph that explains it. |
| **Nothing half-written survives** | Every file lands through `<target>.part` and is renamed only after its byte count matches — and, for a remux, after the result has been re-read from disk and every chunk offset checked against the file it points into. |
| **Resume is not an afterthought** | `--resume` (the default) continues a `.part` with a range request, notices a server that ignores `Range`, and starts over rather than writing the beginning twice. A segment list is resumed per segment: the segments already fetched are cached beside the part with a journal of their lengths, and a rerun asks only for what is missing. |
| **The terminal is the interface** | No config file, no daemon, no environment variables of its own, no state on disk between runs. Every decision is an argument, so the command you ran yesterday is the command that runs today. |

## Roadmap

**No version is tagged until the whole roadmap is done.** The first release will
carry v0.2 through v0.5 in one archive, so until then `main` is the freshest thing
there is and `cargo install --git https://github.com/RUdolf1517/tafas` is how to
get it. The rows below say what is *in the tree*, not what is downloadable —
[roadmap](docs/roadmap.md) has the reasoning.

**Where this is now: v0.4 is in the tree and v0.5 has not started.** v0.2 and
v0.3 are complete in `main` except the playlist/channel *pages* and four
packaging channels listed below; v0.4 finished with per-segment resume,
reproducible and signed releases, the SBOM, budgets, the MSRV job and Windows
long paths. What is left before a tag is v0.5 — a stable `Extractor` trait,
feature-per-site builds and one documentation page per site — plus the v0.2
items that name a URL.

| Where it goes | State | What it is |
| --- | --- | --- |
| [v0.1.0](docs/roadmap.md#1-v010-shipped) | **shipped** | Three sites, DASH remuxing verified by a second reader, atomic `.part` commit, resume, `--json`, the manual page, completions, `--capabilities`. |
| [v0.2](docs/roadmap.md#2-v02-the-shapes-users-actually-hit-done-not-yet-released) | **in the tree** | **In:** HLS → MP4, AES-128 keys, `--cookies`, `--write-info-json`, `--write-thumbnail`, `--write-subs`/`--sub-langs`, `--playlist-items` over HLS segment lists and over list pages (a YouTube playlist or channel, a Vimeo album or showcase, a Twitch collection), an independent test demuxer, `.deb`/`.rpm`, Homebrew, `install.sh`, the per-user Windows installer and the local winget manifests, an Alpine `.apk`, and AUR, Scoop and flake files rendered at release time. **Open:** nothing from this section. |
| [v0.3](docs/roadmap.md#3-v03-reach-and-the-parts-that-are-not-a-url-done-not-yet-released) | **in the tree** | **In:** `--concurrent-fragments`; `--extractor-args`; fragmented MP4 (`EXT-X-MAP`/`moof`); a sample-table reader for a progressive MP4 (`mp4read::plain`); `--embed-subs`, one `wvtt` caption track inside the file; `cargo fuzz` targets, run weekly; Vimeo; Reddit; SoundCloud as MP3; Twitch VODs. **Open:** nothing from the site list. |
| [v0.4](docs/roadmap.md#4-v04-speed-platforms-trust-done-not-yet-released) | **in the tree** | `--limit-rate` and per-segment resume; reproducible builds, an SPDX SBOM and signed checksums; size and startup budgets in CI; an MSRV job; Windows long paths. |
| [v0.5](docs/roadmap.md#5-v05-extensibility-without-a-fork-done-not-yet-released) | **in the tree** | A stable `Extractor` trait, done as a table of readers; feature-per-site builds; one documentation page per site. **The last item before a tag.** |

A version is a plan, not a promise. The promise is
[§7 of the roadmap](docs/roadmap.md#7-what-will-not-happen) — the list of things
this project will not do — together with the rule that an item moves up only when
someone can name the URL that fails without it. Whatever stays unsupported gets a
refusal text in [extractors](docs/extractors.md), because "not yet" is a feature
only if it says so.

## Documentation

| Document | Contents |
| --- | --- |
| [cli](docs/cli.md) | Flags, exit codes, JSON schema, colour and progress, stdout discipline |
| [extractors](docs/extractors.md) | Per-site routes, the exact refusals, and why each one is a refusal |
| [hls](docs/hls.md) | Playlists, the two segment containers, the MP4 writer, and what the pipeline refuses |
| [safety](docs/safety.md) | Atomic commit, resume, path handling, TLS, redirects, limits |
| [dependencies](docs/dependencies.md) | Every dependency, why it exists, its licence, and where the C code is |
| [releases](docs/releases.md) · [roadmap](docs/roadmap.md) | Versioning policy, artifacts, channels · what is planned and what is not |
| [testing](docs/testing.md) | What is tested, how the fixtures are built, what is not covered |
| [CONTRIBUTING](CONTRIBUTING.md) · [SECURITY](SECURITY.md) · [CHANGELOG](CHANGELOG.md) | How to change it · how to report a vulnerability · what changed |

## The name

Hebrew **תפס** (*tafas*): to catch, to seize, to hold on to.

It is what you do with a stream that exists only for as long as the page is
open. The word carries no promise about the file's future: what tafas checks
before it renames a file, and where that check is weaker than it looks, is
written down in [docs/guarantees.md](docs/guarantees.md) rather than implied by
a slogan.

Written by AI. Directed by [RUdolf1517](https://github.com/RUdolf1517) ·
[rudolfzinovev@protonmail.com](mailto:rudolfzinovev@protonmail.com)

## License

[MIT](LICENSE). Dependency licences are checked with `cargo deny` against
[`deny.toml`](deny.toml) when a release is cut; the reasoning for each
dependency, and the rule for adding one, is in
[docs/dependencies.md](docs/dependencies.md).
