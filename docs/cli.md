# Command-line interface

Related: [extractors](extractors.md) · [guarantees](guarantees.md) · [safety](safety.md)

```
tafas [OPTIONS] <URL>...
tafas --list-formats <URL>...
tafas --capabilities | --info | --man | --completions <SHELL> | --help | --version
```

The interface is deliberately small. There is no configuration file, no
environment variable of our own, no daemon and no state carried between runs.
Everything that decides *what happens to a file* is an argument or is on disk.
Two things decide only how the report *looks* — colour and the progress line —
and both are off unless a terminal is attached.

---

## 1. Arguments

| Argument | Behaviour |
| --- | --- |
| a page URL | one item: extracted, selected, downloaded, committed |
| `https://youtu.be/ID`, `vm.tiktok.com/…`, `t.co/…` | short forms are resolved (one redirect follower, at most ten hops) |
| a URL whose host has no reader | exit 1 with `unsupported`, after the other URLs are done |
| `--` | ends options: a URL beginning with `-` needs no quoting games |
| no URL at all | exit 2, with the usage line |

Duplicate URLs are processed twice on purpose: a user who passes the same link
twice is asking for two copies, and silently dropping one of them would make the
output paths unpredictable.

## 2. Destination

| Flag | Meaning |
| --- | --- |
| `-o, --output PATH` | a file for one URL, a directory for several |
| `-o DIR/` | a trailing slash forces the directory reading even for one URL |
| `-o -` | the media bytes on stdout; refused for anything that needs a remux |
| `--name NAME` | stem override; the extension still follows the container |
| `--overwrite` | replace an existing file; without it, an existing destination is an error and **nothing is fetched** |

Default name: `<title> [<id>].<ext>`. The id is part of it because two videos
called "Live" would otherwise overwrite each other, and it is the only stable
handle back to the page. Titles are sanitised before they become paths: `/ \ :
* ? " < > |` and control characters become `_`, runs of whitespace collapse,
trailing dots and spaces are dropped (Windows strips them silently), Windows
device names are prefixed, and the result is bounded at 150 bytes on a character
boundary.

## 3. Selection

| Flag | Meaning |
| --- | --- |
| `-Q, --quality QUALITY` | `best` (default), `worst`, or a height such as `720p`, `1080p`, `480` |
| `--audio-only` | the audio stream alone, as `m4a` when the site has one |
| `--format ID` | one exact stream id from `--list-formats`; requires exactly one URL |
| `--no-mux` | never join; write the streams side by side |
| `--list-formats` | print every stream and stop; nothing is fetched |

`best` compares (height, bitrate, codec preference). The codec preference is
`avc1`/`mp4a` > `av01` > `vp9`/`opus` > anything else, because a file every
player opens beats a smaller file half of them refuse.

A height is a **ceiling**. If no stream is at or below it, the smallest stream
above it is used — the run does not fail on a request the user cannot act on.
`--list-formats` shows the numbers the decision was made from.

Video and audio are joined only when both are ISO-BMFF (`mp4`/`m4a`). A webm
audio stream is downloaded separately and the terminal says why:

A SoundCloud track arrives as an **MP3** and is one of the two files this program writes without rebuilding a container (a poster image is the other). It is copied byte for byte, named `.mp3`, and checked by byte count alone — there is no box structure to verify and nothing here pretends there is.

```
note: the audio stream is opus, which cannot be remuxed with mp4 video without re-encoding
```

### Remuxing

A pair is written as: two `.part` files, a remux into `<target>.part`, a verify
pass that re-reads the result from disk, then one rename. The layout is
`ftyp | moov | mdat(video) | mdat(audio)`; `moov` first so a player can read the
index without seeking to the end (see [guarantees §3](guarantees.md#3-g2--structural-verification)).

| `--write-info-json` | write `<name>.info.json` beside each file |
| `--write-thumbnail` | write the page's poster frame beside each file |
| `--write-subs` | write the page's caption tracks beside each file, as WebVTT |
| `--embed-subs` | put one caption track inside the MP4, as a `wvtt` track |
| `--sub-langs LANGS` | which caption languages: `all` (default), `en`, `pt-BR`, `en*` |

`--write-thumbnail` fetches the poster the page names and puts it beside the
media file, as `<name>.jpg`, `.webp` or `.png` — the extension follows the bytes,
because a `.webp` named `.jpg` is a file half the viewers refuse. Nothing is
resized or converted. It is written after the media file has landed and through
a temporary of its own. Two cases are named rather than silent: a page that
offers no poster, and a photo post, whose images are already files.

`--write-subs` writes one WebVTT file per selected track:
`<name>.<language>.vtt`, or `<name>.<language>.auto.vtt` for a machine
transcript. `--sub-langs` takes `all` (the default when the flag is absent), an
exact code, a language with a trailing `*` — `en*` is `en`, `en-US` and
`en-GB`, but not `eng` — or a comma-separated union of them,
case-insensitively. Three outcomes are told apart:

| Situation | Result |
| --- | --- |
| the page lists no captions and `--sub-langs` was not given | a note, exit 0 — the video is on disk, which is what was asked for |
| `--sub-langs` matches no track | refusal (1), naming the languages the page does offer or `none` |
| a track's body is empty or is not WebVTT | refusal (1) quoting the route — an empty `timedtext` body is a network matter, not a missing language |

`--sub-langs` without `--write-subs` or `--embed-subs` is a usage error (2),
decided before any request: a flag that would do nothing is a mistake, not a
preference. So is giving both writers at once, because one asks for a file beside
the video and the other for a track inside it, and a run that quietly did both
would leave the user guessing which one the flag meant.

The sidecar is written **after** the media file has been renamed into place, so a
failed run never leaves a document describing a file that does not exist. It
carries no media URLs: a signed URL expires within hours and a file that outlives
it should not carry one. Two cases are named rather than silent — `-o -` is a
pipe, so there is nothing to sit beside, and a photo post has one file per image,
so there is no single file the document describes. Both print a note and carry
on.

`--embed-subs` puts the cues **inside** the MP4 instead: one `wvtt` track
(WebVTT in ISO-BMFF), written by the same track builder the muxer uses. The file
the run just produced is read back, the cues become one more `trak`, and the
whole thing is laid out again — samples are passed through byte for byte, so
nothing is re-encoded and the picture does not change. What goes in:

| In the body | In the track |
| --- | --- |
| `00:00:01.500 --> 00:00:03.000` | timescale 1000: the cue time becomes `pts`, the sample lasts 1500 ms |
| a gap before the cue | `stts` cannot state a decode time, only a length, so `dts` is the summed length of the cues before it and the gap becomes a positive `ctts` offset — the cue still appears when the body said |
| the cue text, line breaks kept | a `vttc` box holding a `payl` box with the text |
| cue settings (`align:`, `line:`, `size:`) | dropped: the player places the cue, and `vttc` has no field for them |
| `STYLE` and `REGION` blocks | dropped, for the same reason |
| the track's language | **not** carried: `mdhd` says `und`, and the language lives in the sidecar's name |

One track per file, so `--sub-langs` has to name one language; a selection that
matches several is a refusal (1) that lists them, because two tracks would need
two more `trak`s and a menu the player has to guess at. The gaps between cues are
kept as gaps: a cue states its own duration, so a three-second silence after a
cue is not a five-second cue.

Four cases are named rather than silent: a run that did not produce exactly one
MP4 (two files, `--no-mux`, an audio-only file, a photo post) prints a note and
adds nothing; a page with no caption tracks prints a note; a body with no cues is
a refusal; and `stpp`/TTML, which some pages offer instead of WebVTT, is refused
by name rather than converted.

## 4. Transfer

| Flag | Meaning |
| --- | --- |
| `--no-resume` | do not continue a `.part` (or a segment cache) beside the target; start the transfer over |
| `--retries N` | retries for one request (default 3) |
| `--timeout SECS` | deadline for one metadata request (default 60) |
| `-j, --jobs N` | how many URLs to work on at once (default 1; `0` means four) |
| `--cookies FILE` | send the cookies in a Netscape cookie file (read once, never written back) |
| `--playlist-items ITEMS` | take only these entries of a list: `3`, `1-5`, `2-`, mixed with commas |
| `--concurrent-fragments N` | fetch this many HLS segments at once (`1`–`16`, default `1`) |
| `--limit-rate RATE` | cap a transfer at `RATE` bytes per second (`500K`, `2M`, `1.5M`; default no limit) |
| `--extractor-args SITE:KEY=VALUE` | narrow an extractor's route by hand (repeatable; see below) |

Retried: transport failures, `429`, and `5xx`, with a growing delay (0.4 s,
0.8 s, 1.6 s, 3.2 s, capped at 5 s). Not retried: `400`, `401`, `403`, `404`,
`410`, `416` — those are answers, not weather. A `403` from a CDN is classified
as a refusal, because in practice it is a page check rather than a broken URL.

`--cookies FILE` reads a Netscape cookie file once, at startup. A line that is
not a cookie is skipped; a file with no cookies in it is a usage error (2), and a
file that cannot be read is a failure (1), both before any URL is requested. A
cookie is sent only where it applies — domain (with or without subdomains), path
prefix, `Secure` and expiry are all checked per request — and a `cookie` header
the extractor itself sets (YouTube's consent pair) is kept, with the jar's
cookies appended rather than replacing it.

`--resume` (the default) applies to both shapes of download. A progressive file
continues from its `.part` with a `Range` request; a segment list cannot, because
its `.part` is an MP4 written at the end, so segments are cached beside it
instead (`<target>.part.segments` and a journal of their lengths, [hls §6](hls.md#6-what-is-resumed)).
A rerun replays the cache and asks the server only for the segments that are
missing, and says how many came from the previous run. The cache is dropped when
it describes a different selection — another playlist, another
`--playlist-items`, another build — rather than being spliced into a file it does
not describe. `--no-resume` deletes it before the first request.

`--playlist-items ITEMS` is parsed before the first request, so a typo is a
usage error (2) rather than a half-written file. Entries are 1-based and
inclusive at both ends; `2-` means "from the second to the end". Today the only
list is the segment list of an HLS playlist, where the item is a segment: the
file is assembled from the segments named and from no others, and the playlist
is still read in full, so the count in the refusal is the count the server
served. A selection that matches no segment is a refusal (1); one that does not
start on a keyframe is written, with a warning that the first frames will not
decode.

`--limit-rate RATE` caps the media bytes this run pulls: a progressive body and
every segment of a list are charged against one budget, so `--concurrent-fragments
4` divides the rate rather than multiplying it. `RATE` is bytes per second with an
optional `K`, `M` or `G` suffix in powers of 1024, decimals allowed, checked
before the first request: below `1K` the run would be slower than its own
timeouts, so that is a usage error rather than a clamp, and above `1G` the flag
asks for nothing. Metadata — the page, the JSON, a thumbnail — is not charged;
the budget is for the bytes a shared line is spent on. The limiter is an average
and not a sleep per chunk: each chunk reserves the time it is worth, the worker
waits until its turn, and the wait happens with no lock held so several segments
still overlap. One chunk of slack is deliberate — the first read is never
delayed — so a run can overshoot by a buffer and converges from there.

`--concurrent-fragments N` decides how many segments of one HLS list are in
flight at once; the default, `1`, fetches them one after another, so the flag is
the only thing that changes the order requests are made in. `N` is bounded to
`1`–`16` before the first request, because every worker holds its segment in
memory until the writer has it, and a number the program quietly reduced would be
a lie about what it did. Segments arrive out of order and are written into their
own slot, so the file is the playlist's order however the network interleaves
them; the byte counter behind the progress line moves as segments land, not as
they are written. The first failure cancels the segments still queued — the rest
would be bytes for a file that is not going to exist. Like `--playlist-items`,
it applies to a segment list only: on a progressive download it prints a note and
changes nothing.

`--extractor-args SITE:KEY=VALUE` is the escape hatch for the day a site
changes the document an extractor reads. Its vocabulary is closed and small, and
it is checked before the first request, so a typo is exit 2 with the accepted
spellings in the message rather than a download that quietly took the default
route:

| value | effect |
| --- | --- |
| `youtube:client=NAME` | use only this InnerTube client: `android_vr`, `ios`, `android`, `tv_embedded` (or `tv`), `web`; the watch page is not asked, because its formats are the web client's |
| `tiktok:route=web` | read the page's rehydration document only; the mobile feed is not asked |
| `tiktok:route=app` | ask only the mobile feed; the page is not asked |

The flag is repeatable, one occurrence per site, and the occurrences are folded
into one table. Naming the same key twice — in one occurrence or across two — is
a usage error: the flag is repeatable so that each site gets its own occurrence,
not so that two of them can disagree. A route the flag excludes is named in the
failure the extractor prints, so "the site said no twice" and "you told me to ask
once" never look the same. Every value here is a route the build already has;
nothing can be switched on that a release did not ship, which is why
`--capabilities` lists the whole vocabulary under `extractor_args`.

A media transfer has no total deadline: a 500 MiB file on a slow line is not an
error. A resumed transfer sends `Range: bytes=N-`; a server that answers `200`
instead of `206` is treated as having restarted the body, and the file is
truncated before the second attempt rather than appended twice.

## 5. Exit codes

| Code | Meaning |
| --- | --- |
| `0` | every item succeeded, or the answer was informational |
| `1` | at least one item failed; the others were still captured |
| `2` | invalid usage; no request was made |
| `3` | a fatal error prevented the run from completing |
| `4` | a written file failed verification — treated as a bug in tafas |
| `130` | interrupted by `SIGINT`; a `.part` file may remain |

Precedence is `4` > `2` > `3` > `1` > `0`, applied per item and then folded
across the run: a verification failure in one item is not hidden by an HTTP
error in another. `--dry-run`, `--list-formats` and `--info` are successful
answers and exit 0.

## 6. JSON

`--json` writes one object per item to stdout, one per line, and nothing else.
Schema version 1:

```json
{
  "schema": 1,
  "status": "downloaded",
  "url": "https://youtu.be/aqz-KE-bpKQ",
  "site": "youtube",
  "id": "aqz-KE-bpKQ",
  "title": "Big Buck Bunny",
  "selection": "2160p mp4 + mp4a.40.2 audio (remuxed)",
  "path": "/home/rudolf/Big Buck Bunny [aqz-KE-bpKQ].mp4",
  "bytes": 135_959_660,
  "duration_ms": 596000,
  "elapsed_ms": 18400,
  "reason": null,
  "message": ""
}
```

| Field | Meaning |
| --- | --- |
| `schema` | `1`; a change of meaning bumps it |
| `status` | `downloaded`, `planned`, `listed`, `failed` |
| `site` | `youtube`, `tiktok`, `x`, `vimeo`, or `""` before the page was read |
| `id` | the site's own id for the item |
| `selection` | the human description of what was chosen |
| `path` | the written file, or `null` on failure and for `-o -` |
| `bytes` | bytes written (an estimate with `--dry-run`) |
| `duration_ms` | media duration when the page stated one |
| `elapsed_ms` | wall clock for this item |
| `reason` | stable failure class: `http`, `parse`, `refused`, `unsupported`, `verify`, `limit`, `usage`, `io`, `internal` |
| `message` | the human sentence, empty on success |

`--capabilities` prints a different document, with its own
`capabilities_schema_version`, describing the build: sites, extractor routes,
client chain, container support, transfer behaviour and the two version numbers.
It is read-only — no request, no file access.

## 7. stdout, stderr, and pipes

stdout is data: the written path, one per line, or the JSON objects under
`--json`. Everything else is stderr. That is why `--quiet` suppresses chatter
but not the path: the path is the answer.

A closed stdout pipe ends the run quietly and successfully, the way `head`
expects. `tafas --man | head -1` and `tafas URL | xargs mpv` both behave.

Progress is drawn only when stderr is a terminal, `--quiet`/`--json`/
`--no-progress` are absent, and `-j` is 1. With several jobs the per-item line
takes its place, so two workers cannot draw on top of each other.

## 8. Colour

`--color auto|always|never`, and `--no-color` as `--color never`. In `auto` the
usual precedence applies: `NO_COLOR` (non-empty) disables, `CLICOLOR_FORCE`
(non-empty, not `0`) enables, `CLICOLOR=0` disables, `TERM=dumb` disables, and
otherwise colour is used only when the stream is a terminal.

## 9. Environment

Only what the ecosystem standardises: `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` for
the HTTP client, and the four colour variables above. There is no `TAFAS_*`
variable, because a hidden input is a bug report waiting to happen.

## 10. Conformance notes

- `--help` and `--version` go to stdout and exit 0; errors go to stderr and
  exit 2.
- Every option has a long form; every long form accepts `--opt=value` and
  `--opt value`.
- `-vv` is accepted and counts.
- `man tafas` and `--help` are checked against each other by `tests/packaging.rs`,
  so the page cannot describe a different program.
- Completions are generated from the live parser (`--completions bash`), not
  maintained by hand.
