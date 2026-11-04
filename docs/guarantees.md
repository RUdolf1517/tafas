# Guarantees

Related: [safety](safety.md) · [extractors](extractors.md) · [cli](cli.md)

A downloader earns trust differently from a compressor. It does not transform
bytes, so there is no "equivalent output" to argue about: the claim is that the
file on disk is the file the server sent, that the container holding it is
correct, and that anything the program cannot do is said out loud.

Four classes, each with a method that is checked rather than asserted.

---

## 1. G1 — byte count

**Claim.** A stream is committed only when the number of bytes on disk equals
the number the server declared.

**Method.** The response's `Content-Length` (plus the resume offset, when the
body was a `206`) is compared with the file length after the transfer, before
the rename. A short or long body is a verification failure: the `.part` file
stays for a resume, the destination is never created, and the exit code is 4.

**Where it is weaker than it sounds.** `Content-Length` is what the server says,
not a checksum, and none of the three sites publish one for media. A server that
returns a *complete but wrong* body would pass. That is not a gap this project
can close with the information available — TLS already excludes a third party
changing bytes in flight, so the remaining risk is the server itself, which is
the source of truth by definition.

## 2. G2 — structural verification

**Claim.** A remuxed file is a well-formed ISO-BMFF file that a player can open.

**Method.** After the remux, `mux::verify` re-opens the file **from disk** and
parses it with the same low-level box reader, but with none of the writer's
state:

| Check | Failure |
| --- | --- |
| `ftyp`, `moov` and at least one `mdat` are present, sizes consistent | `verify` |
| every `trak` has `mdia/minf/stbl` with a chunk-offset table | `verify` |
| every chunk offset lies inside an `mdat` payload | `verify` |
| every track's `stsz` sample count is non-zero | `verify` |
| both a `vide` and a `soun` handler are present in a video+audio remux | `verify` |

Any failure exits 4, removes the temporary files and leaves no destination.

**What is not checked.** The samples themselves are not decoded: tafas does not
contain an H.264 or AV1 decoder, and adding one would turn a container tool into
a codec tool with a codec tool's bugs. A file whose sample tables are consistent
but whose samples are corrupt *for the site's own reasons* would pass. The
mitigation is architectural rather than verificational — G3 — because the bytes
are copied, not produced.

## 3. Why the layout makes verification meaningful

```text
┌───────┬──────────────────────┬──────────────────────┬──────────────────────┐
│ ftyp  │ moov                 │ mdat (video payload) │ mdat (audio payload) │
└───────┴──────────────────────┴──────────────────────┴──────────────────────┘
                                    ▲                        ▲
        stco/co64 entries ──────────┘        stco/co64 ──────┘
```

`moov` is written first, which is the only reason a partially transferred file is
still openable — and the only reason the check above is not vacuous: the index
is not derived from the payload after the fact, it is *patched against a layout
computed before the first byte is written*, then verified against the file that
resulted.

## 4. G3 — no re-encoding

**Claim.** Picture and sound are the bytes the site served.

**Method.** The remuxer copies `stts`, `stsc`, `stsz`, `ctts`, `stss`, `stsd`
(and every codec configuration box inside it) verbatim. The only fields it
writes are:

| Field | Why |
| --- | --- |
| `stco`/`co64` entries | chunk offsets are absolute file offsets and the file changed |
| `tkhd.track_id` | two files each start at 1; ids must be unique |
| `tkhd.duration`, `mvhd.duration` | rescaled into the movie timescale |
| `mvhd.next_track_id` | so a later edit does not reuse an id |

There is no encoder in the dependency graph, so this is a property of the code
rather than a discipline: nothing in the build *could* re-encode. `tests` assert
it the only way a test can — by planting a recognisable payload in each source
and finding it unchanged in the output.

## 5. G4 — explicit refusal

**Claim.** Anything not reachable is a named failure.

**Method.** Every extractor path returns either a usable stream list or an
error with a stable `kind`. There is no path that writes a file and then
reports success without a complete commit. The refusal classes are:

| Kind | Meaning | Exit |
| --- | --- | --- |
| `refused` | understood and declined: cipher, age gate, HLS-only, sign-in check | 1 |
| `unsupported` | the site has nothing this build can take: no media, live stream, deleted post | 1 |
| `parse` | the response was not what it claimed to be | 1 |
| `http` | transport, after retries | 1 |
| `limit` | a configured ceiling was exceeded | 1 |
| `verify` | a written file failed G1/G2 | 4 |
| `usage` | the command line was wrong | 2 |

Nothing is skipped quietly, and nothing that fails exits 0.

## 6. Independence, stated plainly

Where the verification is a second reader, it is a second *pass over the file*,
not a second implementation of ISO-BMFF: `remux` and `verify` share the box
parser. That is a real limitation and is stated here rather than hidden behind
the word "verified". The parts that are genuinely independent:

- the payload is compared with the source by the test suite, not by the parser;
- the failure modes the parser can have (a wrong size, a wrong offset) are
  exactly the ones a second pass catches, because the second pass reads the file
  the first one wrote;
- any panic, any out-of-bounds read, is a `verify`/`parse` error before a rename,
  never a half-written destination.

A future version may add an independent demuxer as a dev-dependency for tests
(see [roadmap](roadmap.md) §6). Until then, this paragraph is the honest version
of the claim.

## 7. What is deliberately absent

| Not claimed | Why |
| --- | --- |
| "Always downloads" | the sites decide, not tafas; every refusal in [extractors](extractors.md) is a case where it does not |
| "Reconstructs a deleted video" | it fetches what the site serves now |
| "Verifies the media plays" | that needs a decoder (§2) and a player, and a player is a different program |
| "Same output size as the site" | the remux changes container overhead: two `moov` become one, a second `ftyp` disappears |
| "Resumable on every server" | a server that ignores `Range` restarts the body, and that is detected, not assumed away |
