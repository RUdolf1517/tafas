# Safe file handling

Related: [guarantees](guarantees.md) · [cli](cli.md) · [releases](releases.md)

tafas writes files, follows URLs and parses documents produced by someone else.
Each of those is a place where a wrong decision is expensive, so each has one
written rule.

---

## 1. The commit protocol

Every destination is written twice: once as a temporary, once as itself.

```text
1. <target>.part        created next to the target, never in /tmp
2. body streamed into it
3. flush, fsync(file)
4. byte count compared with the server's declaration      → G1
5. (remux only) mux into <target>.part, verify from disk  → G2
6. rename(<target>.part → <target>)                        ← atomic
7. fsync(parent directory)                                 ← durable
8. (--write-info-json only) the same cycle for <name>.info.json, after step 7
```

Whitespace, not detail: the rename only happens at step 6, so every failure
before it leaves the destination exactly as it was — absent, or the previous
file, untouched. The temporary lives beside the target because `rename(2)` is
atomic only within one filesystem, and `/tmp` is usually another one.

A failed parent-directory `fsync` is reported as a warning and does not change
the outcome: the rename has already been applied, and the data is in the file.
On Windows the directory cannot be opened as a file at all, so that step is a
no-op there.

## 2. Overwrite

`--overwrite` is required to replace an existing file. Without it, an existing
destination is an error **before the request is made**: a download that
overwrites by accident is worse than a download that refuses.

The overwrite check happens twice on purpose — once when the path is computed,
and once immediately before the rename — because a second process may have
created the file in between, and the second check is the one that runs in the
same instruction neighbourhood as the rename.

## 3. Resume

`--resume` (the default) continues an interrupted transfer:

| Situation | Behaviour |
| --- | --- |
| `.part` absent | normal transfer |
| `.part` present, server answers `206` | bytes appended, offsets verified by the byte count |
| `.part` present, server answers `200` | body restarted: the file is truncated first, so the beginning cannot appear twice |
| `.part` present, server answers `416` | treated as "already complete or stale": the transfer restarts from zero |
| `--no-resume` | `.part` is truncated and the transfer starts over |

A `.part` file is never renamed on its own. Stale parts are the user's to
delete, and `--no-resume` is the switch that says so.

## 4. Paths

A page title is attacker-controlled input that becomes a path. The reduction:

- `/ \ : * ? " < > |` and control characters become `_`;
- runs of whitespace collapse to one space; leading and trailing whitespace is
  dropped;
- trailing dots and spaces are dropped, because Windows strips them silently and
  the reported name would differ from the file;
- `.` and `..` cannot survive, since a name is a single path component by
  construction — no title can add a separator;
- Windows device names (`con`, `nul`, `com1`, …) are prefixed with `_`;
- the stem is bounded at 150 **bytes**, cut at a character boundary, so a
  Cyrillic or CJK title is truncated by characters and never mid-codepoint.

The directory part of the name never comes from the page: it comes from `-o`,
`--name` or the current directory. There is no way for a title to place a file
outside the directory the user chose.

Output directories are created with `create_dir_all` when the user named a
directory that does not exist. Nothing else is created, and nothing is deleted
except the temporaries this run made.

## 5. Network

| Decision | Value |
| --- | --- |
| scheme | `http` and `https` only; anything else is a usage error before a request |
| TLS | rustls with the `ring` provider and webpki roots — no system trust store, no OpenSSL |
| redirects | at most ten, then an error; the final URL is what the extractor sees |
| request deadlines | 60 s for metadata (configurable), none for a media body |
| response ceilings | 16 MiB for JSON, 24 MiB for HTML — a page larger than that is not a page we can use |
| HLS segments | one at a time, 64 MiB each at most; decrypted in memory and dropped once the samples are lifted out |
| HLS keys | fetched once per URI and required to be exactly 16 bytes |
| captions | WebVTT only, 16 MiB each; a body that is empty or is not WebVTT is a refusal, not an empty file |
| retries | transport errors, `429`, `5xx`; never `4xx` |
| proxy | `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY` — the client's own handling, not a flag |
| cookies | none of its own: YouTube receives the consent cookie, which is what makes a European request return a page at all; otherwise only what `--cookies FILE` supplies, matched by domain, path, `Secure` and expiry |
| credentials | file names only: a cookie jar is read once and never written back, and no cookie value is logged, printed or quoted in an error; a `user:pass@` in a URL is dropped before use |
| telemetry | none; the only requests are the ones a capture needs |

The user agent is a browser string, because that is what the seven sites serve
data to. That is a statement about the sites, not about the program: tafas does
not execute JavaScript, does not run a headless browser, and does not solve
challenges.

## 6. What tafas never does

- It never runs another program. There is no `Command::new` in the codebase, and
  `tests/packaging.rs` fails if one appears.
- It never decodes or encodes media. There is no codec in the dependency graph.
- It never writes outside the chosen destination directory and its own `.part`
  temporaries.
- It never modifies the file it is replacing until the rename.
- It never follows a URL scheme other than `http(s)`.
- It never stores anything between runs. There is no cache, no config, no state.

## 7. Failure modes and what a user sees

| Failure | Result |
| --- | --- |
| page unreachable | one line on stderr, exit 1, no file |
| page readable, no usable stream | refusal naming the reason, exit 1, no file |
| body truncated mid-transfer | `verify` (byte count), exit 4, `.part` kept for resume |
| disk full | `io`, exit 1, `.part` removed or left partial — never renamed |
| remux refused (fragmented input, offset outside an `mdat`) | `refused`/`parse`, exit 1, temporaries removed, destination untouched |
| remux produced an inconsistent file | `verify`, exit 4, temporaries removed |
| `SIGINT` during transfer | process dies, `.part` remains, exit 130 from the shell, next run resumes |
| destination appears while downloading | `refused` at the rename, exit 1, downloaded bytes discarded |
| stdout pipe closed | silent success; the run stops rather than dying on `SIGPIPE` |

`SIGINT` is not intercepted. The commit protocol is the interrupt handler: a
signal can only leave a `.part` behind, which is exactly what a resumable
download wants.
