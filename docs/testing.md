# Testing

Related: [guarantees](guarantees.md) · [CONTRIBUTING](../CONTRIBUTING.md)

```console
$ cargo test --all-targets
$ cargo clippy --all-targets -- -D warnings
$ cargo fmt --all -- --check
```

## 1. The rule that shapes everything

**No test touches the network.** Not because a request is hard, but because a
test whose result depends on today's YouTube is a test that fails for reasons the
person running it cannot fix. The three extractors are tested against documents
they were designed to read, and the transfer layer is tested against a socket
this repository opens and closes itself.

## 2. Layers

| Layer | Where | What it proves |
| --- | --- | --- |
| unit | `#[cfg(test)]` in each module | URL parsing and rejection, title sanitising, format classification, selection rules, box parsing and patching, the `--extractor-args` vocabulary, exit-code mapping, colour precedence |
| remux | `src/mux.rs` tests | a synthetic two-file remux produces one file with both tracks, both payloads intact, `moov` first, and `verify` accepting it |
| end-to-end | `tests/cli.rs` | the real binary, spawned, against a local HTTP server this test starts on `127.0.0.1:0` |
| sidecar | unit tests in `src/info.rs` | the document names the page, the streams and the files, carries no media URL, and is written through a temporary that is gone after the rename |
| cookies | `tests/cookies.rs` | a jar on disk and a fixture route that answers `403` without the session cookie; host, `Secure` and header-merging rules |
| HLS | `tests/hls.rs` | a playlist, segments, byte ranges and AES-128 segments on a socket, assembled into an MP4 that `mux::verify` accepts; fMP4, short keys and ignored ranges refused before a file exists |
| independent reader | `tests/common/mp4.rs`, `tests/mp4.rs` | a second box walker, sharing no code with `mux` or `mp4write`, that reads the written file and checks every sample offset against `mdat`; tested against a file it builds itself byte by byte |
| packaging | `tests/packaging.rs` | every option in `--help` appears in `man/tafas.1`; no `Command::new` in `src/`; the capability document is valid JSON |

Fixtures are **built by code**, never committed. A reviewer can read what a test
feeds the binary, and adding a case is writing a constructor rather than adding a
binary blob nobody can diff. That is the same rule tzim uses, for the same
reason.

## 3. The remux fixture

`src/mux.rs` builds a one-track MP4 in about a hundred lines: `ftyp`, `moov`
(`mvhd`, `trak` with `tkhd`, `mdhd`, `hdlr`, `minf`/`stbl` with the five tables a
player needs), then `mdat`. The `stco` entry is patched to the real payload
offset after the layout is known, exactly as the writer does it.

The payloads are recognisable byte patterns (`0..100` for video, `0xBB` for
audio), so the test can assert what a checksum cannot: that the exact bytes
survived the remux and are findable in the output.

## 4. What the end-to-end tests cover

| Test | Expectation |
| --- | --- |
| a page with a progressive MP4 | one file, correct bytes, correct name, exit 0 |
| the same run with `--json` | one object, `status: "downloaded"`, `schema: 1` on stdout and nothing else |
| an existing destination | exit 1, nothing fetched, the original untouched |
| the same with `--overwrite` | replaced |
| a page that returns 500 three times | retries, then exit 1 with `reason: "http"` |
| `--dry-run` | no request for media, `status: "planned"` |
| `--list-formats` | the stream table on stdout, no media request |
| an interrupted transfer (`--resume`) | the `.part` continues with a `Range` header, byte count still exact |
| a server that ignores `Range` | the file restarts rather than doubling its beginning |
| a body shorter than `Content-Length` | exit 4, no destination, `.part` kept |
| a closed stdout pipe | exit 0, no panic, no error text |
| `--concurrent-fragments 4` over four segments the server holds for 80 ms | the server saw more than one request at once, and the file is still the four segments in playlist order |
| the same fixtures with the default `1` | the server never saw two requests at once |
| a 404 on the first segment of a parallel run | the queued segments are never requested, and nothing is written |
| `--extractor-args youtube:client=flash` (and five other shapes) | exit 2 before any request, and the message lists the values that would have worked |
| two `--extractor-args` flags, one per site | both are accepted; the same key named twice is exit 2, not a silent last-one-wins |

The local server is a few dozen lines of `std::net::TcpListener`: it parses the
request line and headers, answers a canned body, and can be told to fail a given
number of times first. It serves each connection on its own thread, so a test can
ask for several things at once, and a route can be told to hold its answer for a
moment — the overlap in the parallel test is a fact about the server's clock
rather than an assertion about how long the client took.

## 5. Not covered, and said so

- **The real sites.** A test that logged into TikTok would be a test that breaks
  on a Tuesday. Extraction is verified by the shape of the documents it reads and
  by the refusal paths; a page that has changed produces a refusal, and a bug
  report with that refusal in it is the mechanism that fixes it.
- **Actual playback.** No test decodes a frame. `verify` checks the container,
  not the codec (see [guarantees §2](guarantees.md#2-g2--structural-verification)).
- **Windows ACLs and case-insensitive collisions.** The name sanitiser is tested;
  the filesystem below it is not.
- **HTTP/2-specific behaviour.** The client speaks both; the fixture server
  speaks HTTP/1.1.
- **The Windows installer, from `cargo test`.** `packaging/windows/install.ps1`
  is PowerShell: it moves directories, writes the user `PATH` and rolls back, so
  a Rust test asserting on strings would prove almost nothing. It has its own
  workflow (`windows-installer.yml`), which dot-sources the installer, replaces
  only its three seams — release resolution, download and directory move — and
  drives the real code against archives built in the temp directory. No network,
  no published release. The winget generator is ordinary Python and is tested on
  Ubuntu by `packaging/winget/tests/check.py`.

## 6. Fuzzing

The parsers worth fuzzing are the two that read bytes from someone else:
`mux::scan`/`verify` (ISO-BMFF) and `sites::json_after` (brace matching). Both are
pure functions over a slice, both are `#![forbid(unsafe_code)]`, and both are
written to return an error rather than index out of bounds — which is what a fuzz
target would be asserting.

A fuzz target is planned with the independent-demuxer work in
[roadmap §3](roadmap.md#3-v03--reach-and-the-parts-that-are-not-a-url). Until then, the parser tests
include truncated and padded inputs explicitly.

## 7. CI

| Workflow | Job | What it runs |
| --- | --- | --- |
| `ci.yml` | `test (ubuntu, macos, windows)` | `cargo clippy --all-targets -D warnings`, `cargo test --all-targets`, `cargo build --release` |
| `format.yml` | `format` | `cargo fmt` plus `cargo generate-lockfile`, committed back to the branch |
| `release.yml` | `build (five targets)` | `cargo build --release --locked`, archives, `SHA256SUMS`, packages |
| `windows-installer.yml` | `installer` (windows-latest) | `packaging/windows/tests/Install.Integration.ps1`: install, upgrade, refusal, rollback and uninstall against locally built release fixtures |
| `windows-installer.yml` | `winget-manifests` (ubuntu-latest) | `packaging/winget/tests/check.py`, plus a render of the manifests from a ZIP built the way the release builds it |

Three platforms, one job each, because the failures this project has actually had
were platform-shaped: a path joined with `\`, a file renamed while a reader held
it open, a crypto provider that one target could not pick. The `packaging` checks
run inside the same `cargo test --all-targets` invocation; they need no job of
their own because they are ordinary tests.

`format` exists because this project is partly written in an environment without
a Rust toolchain: a contributor whose environment cannot run `cargo` still gets a
formatted branch and a resolved lockfile, from CI, on the branch they pushed.

The declared floor, `rust-version = "1.82"` in `Cargo.toml`, is not a per-commit
job. A transitive dependency can raise the real floor without a line of this
repository changing, so the floor is re-checked when the lockfile moves and again
when a release is cut, where `cargo deny check` (see `deny.toml`) and
`cargo audit` run in the same pass. A red badge on a dependency's release page is
not something a three-platform matrix should be asked to detect.
