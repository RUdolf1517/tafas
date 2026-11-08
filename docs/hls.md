# HLS

An `.m3u8` is not a video. It is a text file naming other files, and the whole
job of this part of the program is to turn that into one playable MP4 without
re-encoding anything. This page is how that works, what it refuses, and why the
line between the two sits where it does.

It is also the page for the two containers an HLS segment can be: MPEG-TS and
fragmented MP4. They arrive by different routes and leave through one writer.

## 1. From a playlist to a list of segments

`src/hls.rs` reads the text and answers a `Media`: a list of segments with a
duration, a sequence number, an optional byte range, an optional key and an
optional initialisation section. A master playlist is walked to one variant —
`--quality` picks the height, and bandwidth breaks a tie — and the variant's own
list is then read the same way. `EXT-X-MEDIA` audio renditions are followed when
the variant does not carry its own audio.

A playlist **without** `EXT-X-ENDLIST` is a live stream and is refused by name:
there is no finished recording to write, and a file that stops where the run
stopped is a lie about what the stream is.

Every URI is resolved against the playlist it was written in, because a relative
segment URI under a CDN path is the normal case, not the exception.

## 2. Two containers, one writer

The segments are fetched, concatenated, and handed to a demuxer. Which demuxer
is decided by the *playlist*, not by sniffing bytes:

| playlist | container | reader |
| --- | --- | --- |
| no `EXT-X-MAP` | MPEG-TS: 188-byte packets, PAT/PMT, PES | `src/ts.rs` |
| `EXT-X-MAP:URI="init.mp4"` | fragmented MP4: `moof`+`mdat` per segment | `src/mp4read.rs` |

Both readers produce the same thing — `Track`s of `Sample`s, each track with a
timescale and a codec record — and `src/mp4write.rs` lays either one out as
`ftyp`+`moov`+`mdat`. That is why adding fMP4 did not touch the writer: the
container is a transport detail, and the transport ends at the demuxer.

### 2.1 Fragmented MP4

The initialisation segment is fetched **once** for the playlist, before the media
segments, and it is the only part of this pipeline that is not a segment. It
carries `moov`:

* each `trak`'s `tkhd` (the track id the fragments will name) and `mdhd` (the
  timescale);
* the `stsd` sample entry, which is where the codec record lives: `avcC` for
  H.264, and for AAC the `esds` descriptor chain down to the two-byte
  `AudioSpecificConfig`;
* `mvex`/`trex`, the defaults a `tfhd` falls back to when it omits a field.

Each media segment is then a `moof` and an `mdat`. Inside the `moof`, `tfhd`
names the track and where its data starts, `tfdt` says the decode time the
fragment begins at, and each `trun` lists its samples' sizes, durations and
flags. A sample is a keyframe when `sample_is_non_sync_sample` is clear, and its
presentation time is its decode time plus `sample_composition_time_offset`.

Three details are worth naming because they are where hand-written fMP4 readers
usually go wrong:

* **The data offset is relative to the `moof`**, not to the segment. That is why
  a fragment is correct at any position in the concatenation, and why parallel
  fetching cannot corrupt a file: segments are stored in playlist order before
  anything is parsed.
* **Decode times are stated, not implied.** `tfdt` wins; a fragment without one
  continues from the previous fragment's end for that track.
* **Defaults cascade**: `trun` per sample, then `tfhd`, then `trex`. A reader
  that only reads `trun` reads zero-sized samples on most real streams.

## 3. Byte ranges and keys

`EXT-X-BYTERANGE` cuts a segment out of a larger object, in both the absolute
`length@offset` form and the short form that continues where the last range
ended. The range is sent as a `Range` header, and the answer is checked to be
the length the playlist promised.

`EXT-X-KEY` with `METHOD=AES-128` is read: the key is fetched once per URI,
cached for the playlist (not written anywhere), and each segment is decrypted in
memory with AES-128-CBC. The IV is the one the playlist names, or — when it
names none, which is the common case — the segment's media sequence number.
Decryption happens before demuxing, so an encrypted fMP4 playlist works for the
same reason an encrypted transport stream does.

## 4. What is refused

A refusal here is `refused` on stderr, exit 1, and no file: nothing is written
until every segment has been read and the writer has produced something it can
verify.

| shape | why |
| --- | --- |
| `METHOD=SAMPLE-AES`, or fragments with `senc`/`saiz`/`saio` | the encryption is inside the container's framing, which is white-box work rather than a cipher |
| a playlist without `EXT-X-ENDLIST` | live: there is no finished recording to write |
| a playlist that changes its `EXT-X-MAP` part way through | two headers describe two track sets, and one file made of both would be neither |
| HEVC (`hvc1`/`hev1`) or AC-3 (`ac-3`/`ec-3`) sample entries | the writer does not emit those records, and a guessed one is a file that does not play |
| an `AudioSpecificConfig` that is not two bytes of AAC-LC | the same, for audio |
| `EXT-X-MAP` whose bytes are not ISO-BMFF | a named parse error, with the box that failed |
| more than 64 MiB in one segment, or 20 000 segments | the limits, named in the error |

## 5. Limits and memory

| limit | value | why |
| --- | --- | --- |
| one segment | 64 MiB | seconds of video; past this it is a mis-served playlist |
| one playlist | 20 000 segments | 33 hours at six seconds a segment |
| one key file | 1024 bytes | a key URL that answers with a megabyte is not a key |

A segment is held in memory while it is parsed, and its samples are held until
the file is written: this build does not stream HLS to disk as it goes, because
the sample tables of an MP4 written at the end need every sample's size and
time. `--concurrent-fragments N` therefore multiplies memory as well as
throughput, which is why `N` is bounded and documented rather than unbounded.

## 6. What is resumed

An HLS download cannot be resumed from its staging file: `<target>.part` is an
MP4, written once at the end, and a half-written one has no offset worth
continuing from. What is resumable is the *segment stream*, so the segments are
cached beside it:

| File | What is in it |
| --- | --- |
| `<target>.part.segments` | the decrypted segments, concatenated in playlist order |
| `<target>.part.journal` | a header line, then one length per segment, in order |
| `<target>.part.audio.segments`, `.audio.journal` | the same for a separate audio rendition |

The header names the build's cache format, the number of segments in the
selection and an FNV-1a fingerprint of the segment URIs. A different playlist, a
different `--playlist-items`, or a cache written by another build is a *different
download*: the header does not match, and the cache is dropped rather than
spliced into a file it does not describe. Nothing is guessed from a timestamp.

Writing is ordered so that the journal can only ever claim bytes that landed: the
payload is written and synced **first**, and the length is appended after. A run
that dies in between leaves bytes the journal does not claim; recovery cuts the
payload back to the last whole segment, because those bytes belong to a segment
that will be fetched again. A torn length line ends the same way. Segments are
charged in playlist order — a segment that arrives early from a parallel worker
waits for the gap before it — so the cache is always a prefix.

On success the MP4 is written, verified, renamed into place, and *then* the
sidecars are deleted. On failure they are kept: that is the whole point.
`--no-resume` deletes them before the first request, so a cache is never spliced
into a run that did not ask for one.

## 7. What is verified

`tests/hls.rs` runs the whole path over a socket: a playlist, its segments, a
byte range and an AES-128 key, assembled into an MP4 that `mux::verify` accepts.
It does the same for a fragmented-MP4 playlist — a hand-built initialisation
segment and two fragments — and for the refusals above, each of which is
asserted to leave no file behind.

The fMP4 fixtures are built box by box in `tests/common/mod.rs`, next to the
transport-stream fixture, so what the reader is asked to read is visible next to
what the test asserts.
