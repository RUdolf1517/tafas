<div align="center">

# tafas

**תפס** — *tafas*: to catch, to seize, to hold on to

**Catch the video, keep the bytes.**

[![ci](https://github.com/RUdolf1517/tafas/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/RUdolf1517/tafas/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/RUdolf1517/tafas?style=flat-square&color=0b3d91)](https://github.com/RUdolf1517/tafas/releases/latest)
[![rust](https://img.shields.io/badge/rust-1.82%2B-dea584?style=flat-square)](https://www.rust-lang.org/)
[![license](https://img.shields.io/badge/license-MIT-0b3d91?style=flat-square)](LICENSE)

[Install](#install) · [Usage](#usage) · [Sites](#sites) · [Why tafas](#why-tafas) · [Docs](#documentation)

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
anything that does not match.

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

Windows x64 — the archive is `tafas-x86_64-pc-windows-msvc.zip` on
[Releases](https://github.com/RUdolf1517/tafas/releases/latest); unpack it and put
`tafas.exe` on `PATH`. No installer, no elevation, no registry.

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

A site is listed only because its reader exists and its refusals are written
down. What each one can and cannot reach — age gates, PO tokens, signature
ciphers, fMP4 playlists — is one table away in
[docs/extractors.md](docs/extractors.md).

## Why tafas

| | |
| --- | --- |
| **It never re-encodes** | Picture and sound are copied as sample tables; the muxer rewrites chunk offsets, track ids and durations in `moov`, and leaves `avcC`, `av1C`, `vpcC` and `esds` byte for byte. Quality is whatever the site served. |
| **It joins streams itself** | YouTube's 1080p is two files. `tafas` builds the single MP4 in Rust — there is no `ffmpeg` to install, and no subprocess to fail in a way that looks like a network error. |
| **It refuses instead of guessing** | Cipher-protected formats, fMP4 playlists, age-gated pages and posts that need an account each exit with a named reason and a link to the paragraph that explains it. |
| **Nothing half-written survives** | Every file lands through `<target>.part` and is renamed only after its byte count matches — and, for a remux, after the result has been re-read from disk and every chunk offset checked against the file it points into. |
| **Resume is not an afterthought** | `--resume` (the default) continues a `.part` with a range request, notices a server that ignores `Range`, and starts over rather than writing the beginning twice. |
| **The terminal is the interface** | No config file, no daemon, no environment variables of its own, no state on disk between runs. Every decision is an argument, so the command you ran yesterday is the command that runs today. |

## Documentation

| Document | Contents |
| --- | --- |
| [cli](docs/cli.md) | Flags, exit codes, JSON schema, colour and progress, stdout discipline |
| [extractors](docs/extractors.md) | Per-site routes, the exact refusals, and why each one is a refusal |
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
