# Contributing

Thanks for considering a contribution. This project has one rule that outranks
everything else:

> **tafas must never write a file it does not trust, and must never claim a
> capability it does not have.**

That rule decides most review outcomes before any style question comes up.

---

## 1. Before you write code

For anything larger than a typo:

1. Open an issue — a bug report, a site that broke, or a proposal.
2. For a new site, read [docs/extractors.md](docs/extractors.md) and write down
   the route and the refusals **first**. A site is added to the README only when
   its refusals are documented and its tests exist.
3. Wait for agreement on the contract. Code written before the contract is
   usually thrown away, because the contract is what reviewers check against.

A site that cannot be supported without cookies, a browser engine or an external
binary is still worth an issue: a *named refusal* is a feature, and it is what
[docs/extractors.md](docs/extractors.md) is made of.

---

## 2. Development setup

```sh
# Rust 1.82 or newer
rustup toolchain install 1.82
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

If your environment cannot run `cargo` (part of this project is written in one
that cannot), the `format` workflow runs `cargo fmt` and resolves `Cargo.lock`
for you and commits the result to your branch — see
[docs/testing.md §7](docs/testing.md#7-ci).

Optional tooling:

```sh
cargo install cargo-deny cargo-audit --locked
cargo deny check
cargo audit
```

The build needs a C compiler for `ring` (see
[docs/dependencies.md §3](docs/dependencies.md#3-where-the-c-is)); it does not
need `pkg-config`, OpenSSL, `ffmpeg` or Python.

---

## 3. What a pull request must contain

| Change | Required in the same pull request |
| --- | --- |
| Bug fix | a regression test that fails before the fix |
| New site | extractor, [docs/extractors.md](docs/extractors.md) section with the refusal table, unit tests over the document shapes, README row |
| New flag | [docs/cli.md](docs/cli.md) entry, `man/tafas.1` entry, parser test, end-to-end test |
| New dependency | [docs/dependencies.md](docs/dependencies.md) entry answering the six questions, measured size delta, licence check |
| Behaviour change | `CHANGELOG.md` entry |
| Anything touching the commit protocol | tests for every failure mode in [docs/safety.md §7](docs/safety.md#7-failure-modes-and-what-a-user-sees) |
| Anything touching the remuxer | a fixture-based test, and a case that must be *refused* |

Additionally:

- `cargo fmt` and `cargo clippy -D warnings` must be clean;
- CI must be green on Linux, macOS and Windows;
- no new `unsafe` (the crate forbids it);
- no `Command::new` — this binary runs no other program, and
  `tests/packaging.rs` enforces it;
- no dependency added "to save lines of code".

---

## 4. Code style

- Idiomatic Rust, `rustfmt` defaults, edition 2021.
- Prefer plain functions and small enums over trait objects and generics. If a
  new module needs a diagram to explain, it is probably too complex.
- Errors are one `enum Error` with a `Display` implementation; no error-chain
  crates.
- No panics on input data. `unwrap`/`expect` are acceptable only for invariants
  that cannot fail, and must be commented as such.
- No `unsafe`. Ever. If a dependency needs it, that is a discussion, not a
  decision.
- Comments explain *why* — especially in the box parser, where the *what* is four
  magic numbers deep.
- The refusal text is part of the interface: it names the problem, says what
  tafas will not do, and points at the paragraph that explains it. Do not
  shorten a refusal into "failed".

---

## 5. Testing expectations

Every behavioural change needs a test. Specifically:

- a success case **and** a refusal case for anything extracted from a page;
- truncated, oversized and malformed inputs for anything that parses bytes;
- tests must not touch the network, must not depend on the clock, and must clean
  up their temporary directories;
- fixtures are built by code at run time and are never committed, so a reviewer
  can read what a test feeds the binary and a new case is a writer, not a blob.

If you find a case where tafas produced a file a real player rejects, **that is
the highest-priority bug class in the project**. Please include the page URL (or
the site's own document, if you can capture it), the exact command, and what the
player said.

---

## 6. Reporting bugs

Use the issue templates for non-security bugs. For a suspected vulnerability,
follow [SECURITY.md](SECURITY.md) and do not disclose details in a public issue.

For a bad download:

1. Stop using the affected version.
2. Keep the file — if it failed verification it was not committed, so you should
   have a `.part` file; if it succeeded and the file is wrong, keep it.
3. Open an issue titled `bad output: <short description>` with the URL, the
   command, the platform, and the output of `tafas --capabilities`.

A refusal is not a bug by itself: it is the program telling you what it cannot do,
and [docs/extractors.md](docs/extractors.md) should already explain it. A refusal
whose reason is *wrong* is a bug.

---

## 7. Commit messages

Conventional Commits:

```
feat(tiktok): read the mobile feed when the page is walled
fix(mux): patch tkhd durations in the movie timescale
docs: state the HLS refusal in extractors
test(youtube): cover a player response with a cipher only
```

Keep the subject under 72 characters. Explain *why* in the body when the change
is not obvious.

---

## 8. Licence

By contributing you agree that your contribution is licensed under the MIT
licence of this repository (see [LICENSE](LICENSE)). There is no CLA. Do not copy
code from projects with incompatible licences — and note that most downloaders
are GPL, so a copy-paste is a licensing event, not a shortcut.

---

## 9. Review culture

Reviews here are about evidence, not preference. Expect questions like:

- Which guarantee does this affect, and which test proves the new behaviour?
- What happens when the page changes shape — a missing field, a `null`, a
  truncated document?
- What happens on a 4 GiB file, a non-UTF-8 path, a Windows rename failure, a
  server that ignores `Range`?
- Does this refusal name itself, and is the name in the docs?
- What does a user see *exactly*, and does the exit code still tell the truth?

Those questions are not hostility; they are the mechanism by which a downloader
earns trust.
