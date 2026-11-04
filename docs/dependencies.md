# Dependencies

Related: [releases](releases.md) · [safety](safety.md)

Every dependency answers six questions: what it does, why this project cannot do
it in less code than it costs, what it drags in, which licence it carries, what
happens when it is wrong, and what it costs in binary size. A dependency that
cannot answer all six is not added.

`cargo deny` checks licences, banned crates, sources and advisories on every
push. The list below is the reasoning; the CI job is the enforcement.

---

## 1. Direct dependencies

| Crate | Version | Licence | Why |
| --- | --- | --- | --- |
| `clap` | 4.5 | MIT OR Apache-2.0 | argument parsing, GNU-style errors, `--help`, suggestions, `--` handling |
| `clap_complete` | 4.5 | MIT OR Apache-2.0 | completions generated from the live parser |
| `reqwest` | 0.12 (blocking) | MIT OR Apache-2.0 | HTTP/1.1 + HTTP/2, redirects, chunked bodies, gzip, proxies |
| `rustls` | 0.23 (`ring`) | Apache-2.0 OR ISC OR MIT | TLS without OpenSSL |
| `serde_json` | 1 | MIT OR Apache-2.0 | the three sites speak JSON; so does `--json` |

Total: five. A downloader is a small program on purpose — `cargo tree` fits on
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
handling, gzip, HTTP/2 and proxy configuration that are already tested against
real servers, and its blocking API keeps the whole program synchronous — no
`async` runtime, no `tokio` in `main`, no executor to reason about when the only
concurrency is "four URLs at once".

*Cost:* `reqwest` pulls `hyper`, `tokio` (as a dependency of the blocking
client), `h2`, `http`, `url`, `icu_*` (through `idna`) and `tower`. It is by far
the largest thing in the tree, and it is the one dependency an owner might
reasonably want gone. The alternative — `rustls` plus a hand-written HTTP/1.1
client — is about 900 lines of code whose failure mode is silent truncation. That
trade is recorded here rather than hidden.

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

## 2. Transitive dependencies of note

| Crate | Reached through | Note |
| --- | --- | --- |
| `tokio` | `reqwest/blocking` | built but never used as an async runtime by this code; the blocking client drives it internally |
| `hyper`, `h2`, `http` | `reqwest` | HTTP/1.1 and HTTP/2 |
| `webpki-roots` | `reqwest` feature | the CA bundle, compiled in — no system trust store |
| `ring` | `rustls` | see §1.3 |
| `icu_*`, `idna` | `url` | internationalised domain handling |
| `cc` | `ring` build | compiles the vendored C; requires a C compiler at build time, not at run time |

`cargo deny` fails on any new dependency whose licence is not in
`{MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause,
ISC, Unicode-3.0, Zlib, MPL-2.0}`. MPL-2.0 is allowed because it is
file-scoped copyleft and does not affect the licence of this crate.

## 3. Where the C is

| Component | Language | Shipped in the binary |
| --- | --- | --- |
| `tafas` | Rust | — |
| every direct dependency | Rust | — |
| `ring` | C and assembly, vendored | **yes** |
| libc (static musl) | C, linked by the toolchain | **yes**, on Linux |

There is no `libav*`, no OpenSSL, no `libcurl`, no `ffmpeg` and no Python. A
build from source needs a C compiler for `ring`; the released binaries do not.

## 4. Adding a dependency

Open an issue first. A pull request that adds one is expected to contain:

1. an entry in this table with all six answers;
2. the licence and whether it is already in the allow-list;
3. the **measured** binary-size delta (`cargo build --release` before and after,
   the same target and toolchain);
4. what the code looks like without it, in a paragraph — "it saved 30 lines" is
   not an answer;
5. `cargo deny check` output.

The bar is deliberately higher than for a bug fix, because a dependency is the
only change in this repository that outlives the pull request that added it.
