# Dependencies

Related: [releases](releases.md) · [safety](safety.md)

## 0. The rule

**A dependency may be written in something other than Rust only if this project
cannot write it itself, or if no Rust equivalent exists.**

Both halves are deliberately narrow. "It is faster", "it is what everyone uses"
and "it has a Rust wrapper" are not on the list. In practice the rule allows
exactly two things in this repository: `ring`, because constant-time elliptic
curve code is the one kind of program that must not be hand-written, and the C
runtime libc hands every binary anyway. Everything else — argument parsing, HTTP,
JSON, the muxer, the extractors — is Rust, and if a job needs code nobody has
written in Rust yet, the answer is to write it here rather than to link a
compiler toolchain into a static binary.

Every dependency, Rust or not, also answers six questions: what it does, why this
project cannot do it in less code than it costs, what it drags in, which licence
it carries, what happens when it is wrong, and what it costs in binary size. A
dependency that cannot answer all six is not added.

`cargo deny` (`deny.toml`: licences, banned crates, sources, advisories) runs in
CI on every commit, as the `licenses` job, with `--all-features` so that a
dependency reached only from one site's code is checked by the build a reader
actually compiles. It used to say it ran "when a release is cut" — and it ran
nowhere: `release.yml` has never invoked it, and neither had anything else.

`cargo audit` is the one that is deliberately not per-commit: the advisory
database moves independently of this repository, so a build that was perfect on
Friday is red on Monday for a reason nobody here caused, and a badge that turns
red at 03:00 is not a test. `cargo audit` is run by hand when a dependency moves,
and it is the step `CONTRIBUTING.md` names for a change to the graph; it is not
wired into a workflow, and these pages do not claim otherwise. Wiring it in is a
decision about red branches, not a missing line.

---

## 1. Direct dependencies

| Crate | Version | Licence | Why |
| --- | --- | --- | --- |
| `clap` | 4.5 | MIT OR Apache-2.0 | argument parsing, GNU-style errors, `--help`, suggestions, `--` handling |
| `clap_complete` | 4.5 | MIT OR Apache-2.0 | completions generated from the live parser |
| `reqwest` | 0.12 (blocking) | MIT OR Apache-2.0 | HTTP/1.1, redirects, chunked bodies, gzip, proxies |
| `rustls` | 0.23 (`ring`) | Apache-2.0 OR ISC OR MIT | TLS without OpenSSL |
| `serde_json` | 1 | MIT OR Apache-2.0 | the seven sites speak JSON; so does `--json` |
| `aes` | 0.8 | MIT OR Apache-2.0 | AES-128 block cipher, for encrypted HLS segments |
| `cbc` | 0.1 | MIT OR Apache-2.0 | CBC mode and PKCS#7 padding, which is the mode HLS specifies |

Total: seven. A downloader is a small program on purpose — `cargo tree` fits on
one screen, and every transitive crate is one more thing that can change under a
release.

### 1.1 `clap`

*Why not hand-rolled?* A hand-written parser is a page of code that has to
reproduce `--opt=value`, `-abc`, `--`, `-vv`, `--help` layout, suggestions for a
typo, and exit code 2 on misuse. tzim does it by hand because a 1 MiB binary
budget makes that worth it. tafas links TLS and an HTTP stack; argument parsing
is not where its bytes go, and a parser a reader cannot get wrong is worth more
here than a kilobyte.

*What is wrong when it is wrong:* an option is accepted that the documentation
does not describe. Mitigation: `tests/packaging.rs` compares `--help` with the
manual page and the completion script, and `Args::command().debug_assert()`
rejects duplicate flags.

### 1.2 `reqwest` (blocking)

*Why not `ureq` or a hand-written HTTP client?* Three sites, three CDNs, plus
redirectors (`t.co`, `vm.tiktok.com`) and Range requests. `reqwest` gives range
handling, gzip and proxy configuration that are already tested against
real servers, and its blocking API keeps the whole program synchronous — no
`async` runtime, no `tokio` in `main`, no executor to reason about when the only
concurrency is "four URLs at once".

*Cost:* `reqwest` pulls `hyper`, `tokio` (as a dependency of the blocking
client), `http`, `url`, `icu_*` (through `idna`) and `tower`. It is by far the
largest thing in the tree, and it is the one dependency an owner might reasonably
want gone. Two of its optional features are off for size and are named where
they are declared in `Cargo.toml`: `http2` (`h2`, 325 KB in an `opt-level = 3` build) and `charset`
(`encoding_rs` plus `mime`, 31 KB, for a `.text()` this crate never calls).

*The alternative:* `rustls` plus a hand-written HTTP/1.1 client — about 900 lines
of code whose failure mode is silent truncation. That trade is recorded here
rather than hidden.

*Async alternative considered:* `reqwest` with `tokio` and `async` extractors.
Rejected: the concurrency in this program is "a handful of URLs", the blocking
API expresses that with `std::thread`, and the tests stay readable.

### 1.3 `rustls` + `ring`

*Why not the system TLS?* A static binary that links OpenSSL is not static, and
`native-tls` makes the program's trust decisions depend on the machine it runs
on. `rustls` with `webpki-roots` gives one behaviour everywhere.

*Why `ring`?* It is the provider `rustls` documents and ships with; it builds
without a C toolchain on the targets this project ships, and it does not pull
`aws-lc-rs`, which is a much larger body of imported code.

*The honest part:* `ring` is not pure Rust. It contains assembly per architecture
and a small amount of C derived from BoringSSL. **This project's own code is
`#![forbid(unsafe_code)]` and contains no C**; the C that exists is inside a
vendored crypto provider, and this paragraph is where that is admitted rather
than glossed over. A build with zero C in the graph would need a pure-Rust
crypto provider that is not yet as well reviewed; the trade is recorded in
[roadmap](roadmap.md).

### 1.4 `serde_json`

*Why not a hand-written parser?* Because the documents are large, nested, and
produced by someone else. `serde_json` is the reference implementation, it is
`unsafe`-free with `#![forbid]`-compatible settings, and its depth limit and
duplicate-key handling are already the ones a security reviewer would ask about.

*What is not used:* the derive macros. The extractors read `serde_json::Value`
directly, because a missing field must mean "this page changed", not "the whole
document is invalid".

### 1.5 `aes` + `cbc`

*Why not `ring`?* Because `ring` deliberately exposes only AEAD modes — AES-GCM,
ChaCha20-Poly1305 — and the HLS specification names AES-128 in **CBC** mode. This
is the correction to an earlier draft of [roadmap](roadmap.md) that assumed
`ring` would cover encryption; it does not, and it should not be bent to.

*Why not hand-written?* AES is a table lookup and a handful of XORs, and a
hand-written version is exactly the kind of code where a timing side channel
hides in a branch that looks harmless. RustCrypto's `aes` uses a bitsliced,
constant-time implementation; `cbc` supplies the mode and the PKCS#7 padding
an HLS segment carries. Both are pure Rust, so §0 is satisfied without an
argument — this is not a non-Rust dependency, it is two small crates.

*What is wrong when it is wrong:* a segment is written as garbage. Mitigation:
the padding check is treated as the integrity signal and a failure is a refusal,
not a file; `tests/hls.rs` encrypts a fixture and asserts the decrypted result
still demuxes into the same four samples.

## 2. Transitive dependencies of note

| Crate | Reached through | Note |
| --- | --- | --- |
| `tokio` | `reqwest/blocking` | built but never used as an async runtime by this code; the blocking client drives it internally |
| `hyper`, `http` | `reqwest` | HTTP/1.1 only: `h2` is deliberately off, see §4 |
| `webpki-roots` | `reqwest` feature | the CA bundle, compiled in — no system trust store. Data, not code, and `CDLA-Permissive-2.0`, which is why that licence is allowed |
| `ring` | `rustls` | see §1.3 |
| `icu_*`, `idna` | `url` | internationalised domain handling |
| `cc` | `ring` build | compiles the vendored C; requires a C compiler at build time, not at run time |

`cargo deny` fails on any new dependency whose licence is not in
`{MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause,
ISC, Unicode-3.0, Zlib, MPL-2.0, CC0-1.0, CDLA-Permissive-2.0}`. MPL-2.0 is
allowed because it is file-scoped copyleft and does not affect the licence of
this crate. `CDLA-Permissive-2.0` is the licence of the Mozilla certificate
store that `webpki-roots` compiles in: it is data, not code, and it is the entry
the first CI run of this check added — the list had been written from the crates
this project chooses, and the CA bundle is one it inherits.

## 3. Where the C is

| Component | Language | Shipped in the binary |
| --- | --- | --- |
| `tafas` | Rust | — |
| every direct dependency | Rust | — |
| `ring` | C and assembly, vendored | **yes** |
| libc (static musl) | C, linked by the toolchain | **yes**, on Linux |

`ring` is here under §0: reimplementing P-256, AES-GCM and the constant-time
discipline around them is not a downloader's job, and a hand-written version
would be the least trustworthy code in the repository. libc arrives with the
toolchain and cannot be removed from a static binary. Neither is a choice this
project made for convenience.

There is no `libav*`, no OpenSSL, no `libcurl`, no `ffmpeg` and no Python. A
build from source needs a C compiler for `ring`; the released binaries do not.

## 3.1 The fuzz crate, which is not a dependency of the binary

`fuzz/` is its own package: `publish = false`, never built by `cargo build`,
never linked into a release. Its one dependency does not appear in the binary's
graph at all.

| Crate | Version | Licence | Why |
| --- | --- | --- | --- |
| `libfuzzer-sys` | 0.4 | MIT OR Apache-2.0 | the `cargo fuzz` runtime: a corpus, a coverage signal and a crash report |

Nightly-only, dev-only, and the measurement §4 asks for comes out as zero — no
byte of the released binary changed. That is a honest measurement, not a missing
one. The targets themselves are described in [testing §6](testing.md#6-fuzzing).

## 3.2 What the binary weighs, and where it goes

2 878 392 bytes is the recorded budget; an ordinary build on a CI runner today is
2 905 872 (+1.0%), which is drift between runners and days rather than a change
in the graph. Both numbers are `x86_64-unknown-linux-gnu`, `opt-level = "z"`,
fat LTO, `panic = "abort"`, symbols stripped.

`cargo bloat --crates` on that build, as a share of `.text`:

| Crate | `.text` | Share | What it is |
| --- | --- | --- | --- |
| `tafas` | 340.3 KiB | 19.6% | this repository: seven parsers, the remuxers, the writer |
| `std` | 329.7 KiB | 19.0% | allocation, formatting, `std::backtrace` symbolisation, intrinsics |
| `rustls` | 253.4 KiB | 14.6% | the TLS state machine and its message codec |
| `ring` | 179.0 KiB | 10.3% | TLS primitives; the only C and assembly in the graph |
| `reqwest` | 142.7 KiB | 8.2% | the HTTP client |
| `clap_builder` | 108.2 KiB | 6.2% | argument parsing |
| `hyper_util`, `http`, `hyper` | 94.7 KiB | 5.4% | the HTTP/1.1 plumbing under `reqwest` |
| `webpki` + `webpki-roots` | 36.3 KiB | 2.1% | certificate path validation and the CA bundle |
| `clap_complete` | 35.4 KiB | 2.0% | `--completions` for four shells |
| everything else | ~130 KiB | 7.6% | `aes`/`cbc` (6.1 KiB), `miniz_oxide`, `idna`, `serde_json`, … |

Which is the useful way to read it: **most of the binary is not this repository.**
Two thirds of it is TLS, the HTTP client, the argument parser and the standard
library, and every one of those is a correctness argument rather than a fat one —
`ring` is what makes the binary static, `clap` is what prints the errors, and
`rustls` is what keeps OpenSSL out of the build.

### What was tried, and what it bought

Every number below is one build on the same runner, same commit, one variable
changed. They are recorded so that the next person does not spend a day
rediscovering them.

| Change | Bytes | Effect |
| --- | --- | --- |
| `opt-level = "s"` instead of `"z"` | 3 075 328 | **+5.8%:** `"z"` is already the best choice |
| `opt-level = 2` | 3 720 056 | +28.0% |
| `opt-level = 3` | 3 906 504 | +34.4% |
| no `tls12` in `rustls` | 2 905 872 | **exactly the same, byte for byte** — rustls keeps the TLS 1.2 code whether or not the feature is on; the feature only stops it being *reached* |
| no `reqwest` `gzip` (and no `.gzip(true)`) | 2 856 032 | −1.7%, at the cost of every page and every JSON body crossing the wire uncompressed |
| one site only (`--no-default-features --features youtube`) | 2 815 144 | −3.1% for six of the seven readers |

None of these is worth a change on this evidence. The two that *would* pay are
already done and are in the profile above: `opt-level = "z"` and dropping
`http2` and `charset` from `reqwest`.

### Where the next reduction would have to come from

Not from a flag. The remaining candidates each cost something a user can see:

- **`clap` → hand-written parsing** (108 KiB + 35 KiB). The completions are
  generated from the live parser, so they would have to be written by hand and
  would then be able to drift from `--help` — the exact failure this repository
  avoids elsewhere.
- **`aes` + `cbc` → a 120-line AES-128-CBC** (~7 KiB). It is one function
  (`hlsfetch::decrypt`). A hand-written cipher is the one place where a smaller
  binary is a worse program, and the dependency is pure Rust with a known name.
- **Static musl → glibc** (not measured here). The release binary is `musl` so it
  runs on any distribution; a glibc build is smaller and runs on fewer machines.
- **`panic = "abort"`, fat LTO, `strip`** — all three are already on. There is no
  fourth lever in the profile.

## 4. Adding a dependency

Open an issue first. A pull request that adds one is expected to contain:

0. the §0 answer: **could this be written here, and is there a Rust crate that
   already does it?** A non-Rust dependency that fails that question is refused
   before anything else is read;
1. an entry in this table with all six answers;
2. the licence and whether it is already in the allow-list;
3. the **measured** binary-size delta (`cargo build --release` before and after,
   the same target and toolchain);
4. what the code looks like without it, in a paragraph — "it saved 30 lines" is
   not an answer;
5. `cargo deny check` output.

The bar is deliberately higher than for a bug fix, because a dependency is the
only change in this repository that outlives the pull request that added it.
