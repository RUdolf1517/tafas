//! ISO base media file format (`.mp4`, `.m4a`) reader, remuxer and verifier.
//!
//! The supported sites hand out media in two shapes: one progressive MP4 that
//! already carries video and audio (TikTok, X, and YouTube's low resolutions),
//! or two separate MP4 files — one video-only, one audio-only (YouTube's DASH
//! streams above 720p). Turning the second shape into something a player opens
//! means *remuxing*: both sample tables are kept byte for byte, the payloads are
//! concatenated, and the only numbers that change are the absolute file offsets
//! of the chunks, the track ids and the durations.
//!
//! Nothing here decodes or encodes media. Codec configuration boxes (`avcC`,
//! `av1C`, `vpcC`, `esds`) ride inside `stsd` untouched, so the output holds the
//! same frames the site served — a bug in this module can misplace data, but it
//! cannot alter it.
//!
//! The layout written by [`remux`] puts `moov` first on purpose, so a partially
//! downloaded file is still seekable by a player that understands it:
//!
//! ```text
//! ┌───────┬──────────────┬──────────────────┬──────────────────┐
//! │ ftyp  │ moov         │ mdat (video)     │ mdat (audio)     │
//! └───────┴──────────────┴──────────────────┴──────────────────┘
//! ```
//!
//! The assembled `moov` does not change size when its offset *values* change, so
//! it is built once, laid out, and patched in place — which is what makes
//! `moov`-first possible without buffering the media.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::{Error, Result};

/// A box inside a byte slice. `start` is its size field, `payload` is the first
/// byte after the header (8 or 16 bytes depending on the size field), and `end`
/// is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    kind: [u8; 4],
    start: usize,
    payload: usize,
    end: usize,
}

/// A parsed source file: the boxes that matter, and where the media bytes are.
struct Source {
    /// Verbatim `ftyp`, empty when the file has none.
    ftyp: Vec<u8>,
    /// Verbatim `moov`, header included.
    moov: Vec<u8>,
    /// `(payload_start, payload_end)` of every top-level `mdat`, in file order.
    mdats: Vec<(u64, u64)>,
    /// File length, for bounds checks in verification.
    len: u64,
    /// Every top-level box in file order. [`strip`] walks it to rewrite the file
    /// in place; [`remux`] ignores it.
    top: Vec<TopBox>,
}

/// A top-level box, located in the file rather than copied into memory.
#[derive(Debug, Clone, Copy)]
struct TopBox {
    start: u64,
    /// First byte after the header — 8 or 16 bytes in, depending on the size
    /// field, which is what makes this worth recording.
    payload: u64,
    end: u64,
    kind: [u8; 4],
}

/// A table of absolute chunk offsets that has to be rewritten.
struct OffsetTable {
    /// Position of the first entry inside the assembled `moov` payload.
    entries_at: usize,
    count: usize,
    /// 4 for `stco`, 8 for `co64`.
    width: usize,
    /// Index into the two sources: which file the offsets point into.
    source: usize,
}

/// What [`remux`] did.
#[derive(Debug, Clone)]
pub struct MuxReport {
    /// Size of the written file.
    pub bytes: u64,
    pub video_tracks: usize,
    pub audio_tracks: usize,
    /// Movie duration in milliseconds, when the movie header states one.
    pub duration_ms: Option<u64>,
}

/// One track, as [`verify`] read it back from disk.
#[derive(Debug, Clone)]
pub struct TrackCheck {
    pub handler: String,
    pub samples: u64,
    pub chunks: u64,
    pub duration_ms: Option<u64>,
}

/// What [`verify`] found.
#[derive(Debug, Clone)]
pub struct VerifyReport {
    pub bytes: u64,
    pub tracks: Vec<TrackCheck>,
}

impl VerifyReport {
    pub fn has_handler(&self, handler: &str) -> bool {
        self.tracks.iter().any(|t| t.handler == handler)
    }

    pub fn sample_total(&self) -> u64 {
        self.tracks.iter().map(|t| t.samples).sum()
    }
}

// ---------------------------------------------------------------------------
// reading helpers
// ---------------------------------------------------------------------------

fn be_u32(data: &[u8], at: usize) -> Result<u32> {
    let slice = data
        .get(at..at + 4)
        .ok_or_else(|| Error::Parse(format!("mp4: truncated u32 at {at}")))?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn be_u64(data: &[u8], at: usize) -> Result<u64> {
    let slice = data
        .get(at..at + 8)
        .ok_or_else(|| Error::Parse(format!("mp4: truncated u64 at {at}")))?;
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(slice);
    Ok(u64::from_be_bytes(bytes))
}

fn put_u32(data: &mut [u8], at: usize, value: u32) -> Result<()> {
    let slice = data
        .get_mut(at..at + 4)
        .ok_or_else(|| Error::Internal(format!("mp4: write past the end at {at}")))?;
    slice.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn put_u64(data: &mut [u8], at: usize, value: u64) -> Result<()> {
    let slice = data
        .get_mut(at..at + 8)
        .ok_or_else(|| Error::Internal(format!("mp4: write past the end at {at}")))?;
    slice.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

/// Splits the boxes that occupy `data[from..to]`.
fn children(data: &[u8], from: usize, to: usize) -> Result<Vec<Span>> {
    let mut spans = Vec::new();
    let mut at = from;
    while at + 8 <= to {
        let size32 = be_u32(data, at)?;
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        let (header, size) = match size32 {
            0 => (8usize, (to - at) as u64),
            1 => {
                if at + 16 > to {
                    return Err(Error::Parse(format!(
                        "mp4: truncated 64-bit header at {at}"
                    )));
                }
                (16usize, be_u64(data, at + 8)?)
            }
            n if n < 8 => {
                return Err(Error::Parse(format!(
                    "mp4: box {} at {at} declares an impossible size {n}",
                    show(&kind)
                )))
            }
            n => (8usize, n as u64),
        };
        let size = usize::try_from(size)
            .map_err(|_| Error::Parse(format!("mp4: box size at {at} does not fit in memory")))?;
        if size < header || at + size > to {
            return Err(Error::Parse(format!(
                "mp4: box {} at {at} runs past its container",
                show(&kind)
            )));
        }
        spans.push(Span {
            kind,
            start: at,
            payload: at + header,
            end: at + size,
        });
        at += size;
    }
    Ok(spans)
}

fn show(kind: &[u8; 4]) -> String {
    format!("{:?}", String::from_utf8_lossy(kind))
}

fn find_child(data: &[u8], span: Span, kind: &[u8; 4]) -> Result<Option<Span>> {
    for child in children(data, span.payload, span.end)? {
        if &child.kind == kind {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

/// Walks a path of box types from `span`.
fn descend(data: &[u8], span: Span, path: &[&[u8; 4]]) -> Result<Option<Span>> {
    let mut current = span;
    for kind in path {
        match find_child(data, current, kind)? {
            Some(child) => current = child,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

fn require(data: &[u8], span: Span, path: &[&[u8; 4]], what: &str) -> Result<Span> {
    descend(data, span, path)?.ok_or_else(|| {
        Error::Parse(format!(
            "mp4: {what} has no {} box",
            path.iter()
                .map(|kind| String::from_utf8_lossy(*kind).into_owned())
                .collect::<Vec<_>>()
                .join("/")
        ))
    })
}

// ---------------------------------------------------------------------------
// source scanning
// ---------------------------------------------------------------------------

fn scan(path: &Path) -> Result<Source> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut reader = BufReader::new(file);

    let mut ftyp = Vec::new();
    let mut moov: Option<Vec<u8>> = None;
    let mut mdats = Vec::new();
    let mut top = Vec::new();
    let mut pos = 0u64;

    while pos + 8 <= len {
        reader.seek(SeekFrom::Start(pos))?;
        let mut header = [0u8; 16];
        reader.read_exact(&mut header[..8])?;
        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let kind = [header[4], header[5], header[6], header[7]];
        let (header_len, size) = match size32 {
            0 => (8u64, len - pos),
            1 => {
                reader.read_exact(&mut header[8..16])?;
                (
                    16u64,
                    u64::from_be_bytes([
                        header[8], header[9], header[10], header[11], header[12], header[13],
                        header[14], header[15],
                    ]),
                )
            }
            n if n < 8 => {
                return Err(Error::Parse(format!(
                    "mp4: {}: box {} at {pos} declares size {n}",
                    path.display(),
                    show(&kind)
                )))
            }
            n => (8u64, n as u64),
        };
        if size < header_len || size > len - pos {
            return Err(Error::Parse(format!(
                "mp4: {}: box {} at {pos} has an impossible size {size}",
                path.display(),
                show(&kind)
            )));
        }

        top.push(TopBox {
            start: pos,
            payload: pos + header_len,
            end: pos + size,
            kind,
        });
        match &kind {
            b"ftyp" => {
                // The header has already been consumed; rewind so the stored
                // bytes are the whole box, header included.
                reader.seek(SeekFrom::Start(pos))?;
                let mut bytes = vec![0u8; size as usize];
                reader.read_exact(&mut bytes)?;
                ftyp = bytes;
            }
            b"moov" => {
                if moov.is_some() {
                    return Err(Error::Refused(
                        "mp4: more than one moov box; not a shape this build remuxes".into(),
                    ));
                }
                reader.seek(SeekFrom::Start(pos))?;
                let mut bytes = vec![0u8; size as usize];
                reader.read_exact(&mut bytes)?;
                moov = Some(bytes);
            }
            b"mdat" => mdats.push((pos + header_len, pos + size)),
            // A fragmented file has no sample tables to copy: remuxing it would
            // mean rewriting fragments, which this build does not do.
            b"moof" => {
                return Err(Error::Refused(format!(
                    "mp4: {} is fragmented (has a moof box); tafas remuxes only plain ISO-BMFF",
                    path.display()
                )))
            }
            _ => {}
        }

        pos += size;
    }

    if mdats.is_empty() {
        return Err(Error::Parse(format!(
            "mp4: {} has no mdat box",
            path.display()
        )));
    }
    let moov =
        moov.ok_or_else(|| Error::Parse(format!("mp4: {} has no moov box", path.display())))?;

    Ok(Source {
        ftyp,
        moov,
        mdats,
        len,
        top,
    })
}

fn moov_span(source: &Source) -> Span {
    Span {
        kind: *b"moov",
        start: 0,
        payload: 8,
        end: source.moov.len(),
    }
}

fn trak_count(source: &Source) -> Result<usize> {
    let span = moov_span(source);
    Ok(children(&source.moov, span.payload, span.end)?
        .iter()
        .filter(|child| &child.kind == b"trak")
        .count())
}

// ---------------------------------------------------------------------------
// header fields, expressed relative to the payload of each box
// ---------------------------------------------------------------------------

/// Movie timescale and the positions of the two fields that need rewriting.
struct MovieHeader {
    timescale: u32,
    duration_at: usize,
    duration_width: usize,
    next_track_id_at: usize,
}

fn movie_header(moov: &[u8], span: Span) -> Result<MovieHeader> {
    let version = *moov
        .get(span.payload)
        .ok_or_else(|| Error::Parse("mp4: truncated mvhd".into()))?;
    match version {
        0 => Ok(MovieHeader {
            timescale: be_u32(moov, span.payload + 12)?,
            duration_at: span.payload + 16,
            duration_width: 4,
            next_track_id_at: span.payload + 96,
        }),
        1 => Ok(MovieHeader {
            timescale: be_u32(moov, span.payload + 20)?,
            duration_at: span.payload + 24,
            duration_width: 8,
            next_track_id_at: span.payload + 108,
        }),
        v => Err(Error::Parse(format!("mp4: mvhd version {v} is not known"))),
    }
}

/// Positions of the `tkhd` fields that are rewritten, plus the media timescale
/// and duration from `mdhd` (which stays as it is).
struct TrackHeader {
    track_id_at: usize,
    duration_at: usize,
    duration_width: usize,
    timescale: u32,
    duration: u64,
}

fn track_header(trak: &[u8], span: Span) -> Result<TrackHeader> {
    let tkhd = require(trak, span, &[b"tkhd"], "trak")?;
    let version = *trak
        .get(tkhd.payload)
        .ok_or_else(|| Error::Parse("mp4: truncated tkhd".into()))?;
    let (track_id_at, duration_at, duration_width) = match version {
        0 => (tkhd.payload + 12, tkhd.payload + 20, 4),
        1 => (tkhd.payload + 20, tkhd.payload + 28, 8),
        v => return Err(Error::Parse(format!("mp4: tkhd version {v} is not known"))),
    };

    let mdhd = require(trak, span, &[b"mdia", b"mdhd"], "trak")?;
    let mdhd_version = *trak
        .get(mdhd.payload)
        .ok_or_else(|| Error::Parse("mp4: truncated mdhd".into()))?;
    let (timescale_at, duration_at_md, duration_width_md) = match mdhd_version {
        0 => (mdhd.payload + 12, mdhd.payload + 16, 4),
        1 => (mdhd.payload + 20, mdhd.payload + 24, 8),
        v => return Err(Error::Parse(format!("mp4: mdhd version {v} is not known"))),
    };
    let timescale = be_u32(trak, timescale_at)?;
    let duration = if duration_width_md == 4 {
        be_u32(trak, duration_at_md)? as u64
    } else {
        be_u64(trak, duration_at_md)?
    };

    Ok(TrackHeader {
        track_id_at: track_id_at - span.start,
        duration_at: duration_at - span.start,
        duration_width,
        timescale,
        duration,
    })
}

fn handler_type(trak: &[u8], span: Span) -> Result<String> {
    let hdlr = require(trak, span, &[b"mdia", b"hdlr"], "trak")?;
    let bytes = trak
        .get(hdlr.payload + 8..hdlr.payload + 12)
        .ok_or_else(|| Error::Parse("mp4: truncated hdlr".into()))?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

// ---------------------------------------------------------------------------
// remux
// ---------------------------------------------------------------------------

/// Combines a video-only MP4 and an audio-only MP4 into one MP4.
///
/// Sample tables are copied verbatim; chunk offsets are translated from the
/// source file into the new layout, track ids are renumbered, and the durations
/// are rescaled into the movie timescale. Fragmented input is refused, and any
/// inconsistency (an offset outside every `mdat`, a track without a chunk
/// table) is an error rather than a best guess.
pub fn remux(video: &Path, audio: &Path, out: &Path) -> Result<MuxReport> {
    let video_source = scan(video).map_err(|e| add_path(e, video))?;
    let audio_source = scan(audio).map_err(|e| add_path(e, audio))?;
    let sources = [&video_source, &audio_source];
    let paths = [video, audio];

    // --- the movie header comes from the video file ----------------------
    let video_moov = moov_span(&video_source);
    let mvhd_span = require(&video_source.moov, video_moov, &[b"mvhd"], "moov")?;
    let movie = movie_header(&video_source.moov, mvhd_span)?;
    if movie.timescale == 0 {
        return Err(Error::Parse("mp4: mvhd timescale is zero".into()));
    }
    let mvhd_box = video_source.moov[mvhd_span.start..mvhd_span.end].to_vec();

    // --- assemble the new moov payload -----------------------------------
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(&mvhd_box);

    let mut tables: Vec<OffsetTable> = Vec::new();
    let mut track_id_patches: Vec<(usize, u32)> = Vec::new();
    let mut duration_patches: Vec<(usize, usize, u64)> = Vec::new();
    let mut traks_written = 0usize;
    let mut movie_duration = 0u64;

    for (index, source) in sources.iter().enumerate() {
        let span = moov_span(source);
        let traks: Vec<Span> = children(&source.moov, span.payload, span.end)?
            .into_iter()
            .filter(|child| &child.kind == b"trak")
            .collect();
        if traks.is_empty() {
            return Err(Error::Parse(format!(
                "mp4: {} has no track",
                paths[index].display()
            )));
        }

        for trak_span in traks {
            let raw = &source.moov[trak_span.start..trak_span.end];
            let local = Span {
                kind: *b"trak",
                start: 0,
                payload: trak_span.payload - trak_span.start,
                end: raw.len(),
            };
            let header = track_header(raw, local)?;
            if header.timescale == 0 {
                return Err(Error::Parse(format!(
                    "mp4: {} has a track with a zero timescale",
                    paths[index].display()
                )));
            }
            traks_written += 1;
            let new_id = traks_written as u32;
            movie_duration =
                movie_duration.max(scale(header.duration, header.timescale, movie.timescale));

            let base = payload.len();
            payload.extend_from_slice(raw);

            track_id_patches.push((base + header.track_id_at, new_id));
            duration_patches.push((
                base + header.duration_at,
                header.duration_width,
                scale(header.duration, header.timescale, movie.timescale),
            ));

            let stbl = require(raw, local, &[b"mdia", b"minf", b"stbl"], "trak")?;
            let mut found_table = false;
            for child in children(raw, stbl.payload, stbl.end)? {
                let width = match &child.kind {
                    b"stco" => 4,
                    b"co64" => 8,
                    _ => continue,
                };
                found_table = true;
                let count = be_u32(raw, child.payload + 4)? as usize;
                let entries_at = child.payload + 8;
                if count
                    .checked_mul(width)
                    .and_then(|bytes| entries_at.checked_add(bytes))
                    .map(|needed| needed > child.end)
                    .unwrap_or(true)
                {
                    return Err(Error::Parse(format!(
                        "mp4: {} table claims {count} entries and does not fit",
                        show(&child.kind)
                    )));
                }
                tables.push(OffsetTable {
                    // `children` reports positions through the slice it was
                    // given, so `entries_at` is already relative to this trak
                    // and only the trak's place in the new payload is added.
                    entries_at: base + entries_at,
                    count,
                    width,
                    source: index,
                });
            }
            if !found_table {
                return Err(Error::Refused(format!(
                    "mp4: track {new_id} in {} has no chunk offset table",
                    paths[index].display()
                )));
            }
        }
    }

    // --- layout ----------------------------------------------------------
    let out_ftyp = build_ftyp(&video_source.ftyp);
    let moov_size = 8u64
        .checked_add(payload.len() as u64)
        .ok_or_else(|| Error::Limit("mp4: the movie header is too large".into()))?;
    if moov_size > u32::MAX as u64 {
        return Err(Error::Limit(
            "mp4: the movie header does not fit in a 32-bit box size".into(),
        ));
    }
    let ftyp_len = out_ftyp.len() as u64;

    let video_payload_len = payload_len(&video_source);
    let audio_payload_len = payload_len(&audio_source);

    let video_mdat_at = ftyp_len + moov_size;
    let video_header = mdat_header_len(video_payload_len);
    let video_payload_at = video_mdat_at + video_header;
    let audio_mdat_at = video_payload_at + video_payload_len;
    let audio_header = mdat_header_len(audio_payload_len);
    let audio_payload_at = audio_mdat_at + audio_header;

    let mut bases: Vec<Vec<u64>> = Vec::with_capacity(2);
    for (source, payload_at) in [
        (&video_source, video_payload_at),
        (&audio_source, audio_payload_at),
    ] {
        let first = source.mdats[0].0;
        bases.push(
            source
                .mdats
                .iter()
                .map(|(start, _)| payload_at + (start - first))
                .collect(),
        );
    }

    // --- patch -----------------------------------------------------------
    for table in &tables {
        for entry in 0..table.count {
            let at = table.entries_at + entry * table.width;
            let original = if table.width == 4 {
                be_u32(&payload, at)? as u64
            } else {
                be_u64(&payload, at)?
            };
            let mapped = map_offset(original, sources[table.source], &bases[table.source])?;
            if table.width == 4 {
                let mapped = u32::try_from(mapped).map_err(|_| {
                    Error::Limit(
                        "mp4: the remuxed file would need 64-bit chunk offsets (over 4 GiB with \
                         stco); refusing rather than truncating"
                            .into(),
                    )
                })?;
                put_u32(&mut payload, at, mapped)?;
            } else {
                put_u64(&mut payload, at, mapped)?;
            }
        }
    }

    for (at, new_id) in &track_id_patches {
        put_u32(&mut payload, *at, *new_id)?;
    }
    for (at, width, value) in &duration_patches {
        if *width == 4 {
            put_u32(&mut payload, *at, (*value).min(u32::MAX as u64) as u32)?;
        } else {
            put_u64(&mut payload, *at, *value)?;
        }
    }

    // The movie header was appended first, so its fields are at payload offset
    // zero plus whatever `movie_header` reported as an absolute position in the
    // source `moov`.
    let mvhd_base = 0usize;
    let shift = |absolute: usize| absolute - mvhd_span.start;
    if movie.duration_width == 4 {
        put_u32(
            &mut payload,
            mvhd_base + shift(movie.duration_at),
            movie_duration.min(u32::MAX as u64) as u32,
        )?;
    } else {
        put_u64(
            &mut payload,
            mvhd_base + shift(movie.duration_at),
            movie_duration,
        )?;
    }
    put_u32(
        &mut payload,
        mvhd_base + shift(movie.next_track_id_at),
        traks_written as u32 + 1,
    )?;

    if 8 + payload.len() as u64 != moov_size {
        return Err(Error::Internal(
            "mp4: patching changed the size of the movie header".into(),
        ));
    }

    // --- write -----------------------------------------------------------
    let file = File::create(out)?;
    let mut writer = BufWriter::with_capacity(1 << 20, file);
    writer.write_all(&out_ftyp)?;
    writer.write_all(&(moov_size as u32).to_be_bytes())?;
    writer.write_all(b"moov")?;
    writer.write_all(&payload)?;
    copy_mdats(&mut writer, video, &video_source, video_payload_len)?;
    copy_mdats(&mut writer, audio, &audio_source, audio_payload_len)?;
    writer.flush()?;

    let file = writer.into_inner().map_err(|e| Error::Io(e.into_error()))?;
    file.sync_all()?;
    let bytes = file.metadata()?.len();

    Ok(MuxReport {
        bytes,
        video_tracks: trak_count(&video_source)?,
        audio_tracks: trak_count(&audio_source)?,
        duration_ms: Some(movie_duration * 1000 / movie.timescale as u64),
    })
}

fn add_path(error: Error, path: &Path) -> Error {
    match error {
        Error::Parse(m) => Error::Parse(format!("{}: {m}", path.display())),
        Error::Refused(m) => Error::Refused(format!("{}: {m}", path.display())),
        other => other,
    }
}

fn payload_len(source: &Source) -> u64 {
    source.mdats.iter().map(|(start, end)| end - start).sum()
}

/// Converts `duration` from `from` ticks to `to` ticks without overflowing.
fn scale(duration: u64, from: u32, to: u32) -> u64 {
    if from == 0 {
        return 0;
    }
    let scaled = (duration as u128) * (to as u128) / (from as u128);
    scaled.min(u64::MAX as u128) as u64
}

fn mdat_header_len(payload_len: u64) -> u64 {
    if payload_len + 8 > u32::MAX as u64 {
        16
    } else {
        8
    }
}

/// Translates an absolute source offset into the output file.
fn map_offset(offset: u64, source: &Source, bases: &[u64]) -> Result<u64> {
    for (index, (start, end)) in source.mdats.iter().enumerate() {
        if offset >= *start && offset < *end {
            return Ok(bases[index] + (offset - start));
        }
    }
    Err(Error::Refused(format!(
        "mp4: a chunk offset ({offset}) points outside every mdat box; not a layout this build \
         remuxes"
    )))
}

fn copy_mdats(
    writer: &mut BufWriter<File>,
    path: &Path,
    source: &Source,
    payload_len: u64,
) -> Result<()> {
    if mdat_header_len(payload_len) == 16 {
        writer.write_all(&1u32.to_be_bytes())?;
        writer.write_all(b"mdat")?;
        writer.write_all(&(payload_len + 16).to_be_bytes())?;
    } else {
        writer.write_all(&((payload_len + 8) as u32).to_be_bytes())?;
        writer.write_all(b"mdat")?;
    }

    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 1 << 20];
    for (start, end) in &source.mdats {
        file.seek(SeekFrom::Start(*start))?;
        let mut left = end - start;
        while left > 0 {
            let want = buffer.len().min(left as usize);
            file.read_exact(&mut buffer[..want])?;
            writer.write_all(&buffer[..want])?;
            left -= want as u64;
        }
    }
    Ok(())
}

/// Keeps the source brand: `isom`/`mp42`/`avc1` tell a player which features to
/// expect, and inventing a brand would be a guess.
fn build_ftyp(source: &[u8]) -> Vec<u8> {
    if source.len() >= 16 {
        return source.to_vec();
    }
    let mut out = Vec::with_capacity(20);
    out.extend_from_slice(&20u32.to_be_bytes());
    out.extend_from_slice(b"ftyp");
    out.extend_from_slice(b"isom");
    out.extend_from_slice(&0x200u32.to_be_bytes());
    out.extend_from_slice(b"isomiso2");
    out
}

// ---------------------------------------------------------------------------
// verification
// ---------------------------------------------------------------------------

/// Re-reads a file that was just written and checks the properties the remuxer
/// claims: a movie with tracks, every chunk offset inside a payload, and a
/// non-empty sample count per track.
///
/// This is deliberately a second reader: it parses from disk with the same
/// low-level helpers but none of the writer's state, so a writer bug that
/// misplaced a payload is caught here rather than by a player.
pub fn verify(path: &Path) -> Result<VerifyReport> {
    let source = scan(path)?;
    let moov = &source.moov;
    let span = moov_span(&source);

    let mut tracks = Vec::new();
    for child in children(moov, span.payload, span.end)? {
        if &child.kind != b"trak" {
            continue;
        }
        let handler = handler_type(moov, child)?;

        let mut samples = 0u64;
        let mut chunks = 0u64;
        let stbl = require(moov, child, &[b"mdia", b"minf", b"stbl"], "trak")?;
        for leaf in children(moov, stbl.payload, stbl.end)? {
            match &leaf.kind {
                b"stsz" => {
                    let uniform = be_u32(moov, leaf.payload + 4)?;
                    let count = be_u32(moov, leaf.payload + 8)? as u64;
                    // A non-zero sample_size means every sample is that size;
                    // an empty track has neither.
                    if count > 0 && (uniform > 0 || leaf.payload + 12 < leaf.end) {
                        samples += count;
                    }
                }
                b"stco" | b"co64" => {
                    let width = if &leaf.kind == b"stco" { 4 } else { 8 };
                    let count = be_u32(moov, leaf.payload + 4)? as usize;
                    chunks += count as u64;
                    for entry in 0..count {
                        let at = leaf.payload + 8 + entry * width;
                        let offset = if width == 4 {
                            be_u32(moov, at)? as u64
                        } else {
                            be_u64(moov, at)?
                        };
                        if !source
                            .mdats
                            .iter()
                            .any(|(start, end)| offset >= *start && offset < *end)
                        {
                            return Err(Error::Verify(format!(
                                "{}: a chunk of the {handler} track points at {offset}, outside \
                                 every mdat payload",
                                path.display()
                            )));
                        }
                    }
                }
                _ => {}
            }
        }
        if samples == 0 {
            return Err(Error::Verify(format!(
                "{}: the {handler} track has no samples",
                path.display()
            )));
        }

        let mdhd = require(moov, child, &[b"mdia", b"mdhd"], "trak")?;
        let version = *moov.get(mdhd.payload).unwrap_or(&0);
        let (timescale, duration) = if version == 1 {
            (
                be_u32(moov, mdhd.payload + 20)?,
                be_u64(moov, mdhd.payload + 24)?,
            )
        } else {
            (
                be_u32(moov, mdhd.payload + 12)?,
                be_u32(moov, mdhd.payload + 16)? as u64,
            )
        };

        tracks.push(TrackCheck {
            handler,
            samples,
            chunks,
            duration_ms: if timescale == 0 {
                None
            } else {
                Some(duration * 1000 / timescale as u64)
            },
        });
    }

    if tracks.is_empty() {
        return Err(Error::Verify(format!(
            "{}: the movie header contains no track",
            path.display()
        )));
    }

    Ok(VerifyReport {
        bytes: source.len,
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 8);
        out.extend_from_slice(&((payload.len() as u32) + 8).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn full(payload: &[u8]) -> Vec<u8> {
        // version 0, flags 0, then the payload
        let mut body = vec![0u8; 4];
        body.extend_from_slice(payload);
        body
    }

    fn u32s(values: &[u32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(values.len() * 4);
        for value in values {
            out.extend_from_slice(&value.to_be_bytes());
        }
        out
    }

    /// Builds a one-track MP4 whose single chunk is `payload`, laid out as
    /// ftyp | moov | mdat with a correct `stco` entry.
    fn one_track_mp4(handler: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"isom\x00\x00\x02\x00isomiso2");

        let mut tkhd = vec![0u8; 92];
        tkhd[0..4].copy_from_slice(&92u32.to_be_bytes());
        tkhd[4..8].copy_from_slice(b"tkhd");
        tkhd[8] = 0;
        tkhd[11] = 3;
        tkhd[20..24].copy_from_slice(&1u32.to_be_bytes()); // track id
        tkhd[28..32].copy_from_slice(&1000u32.to_be_bytes()); // duration

        let mut mdhd = vec![0u8; 32];
        mdhd[0..4].copy_from_slice(&32u32.to_be_bytes());
        mdhd[4..8].copy_from_slice(b"mdhd");
        mdhd[20..24].copy_from_slice(&1000u32.to_be_bytes()); // timescale
        mdhd[24..28].copy_from_slice(&1000u32.to_be_bytes()); // duration

        // `full` adds the version and flags, so the handler type sits four
        // bytes into the body and eight bytes into the box's payload.
        let mut hdlr_body = vec![0u8; 20];
        hdlr_body[4..8].copy_from_slice(handler);
        let hdlr = bx(b"hdlr", &full(&hdlr_body));

        let mut stsd_body = vec![0u8; 4];
        stsd_body.extend_from_slice(&1u32.to_be_bytes());
        stsd_body.extend_from_slice(&bx(b"mp4v", &[0u8; 8]));
        let stsd = bx(b"stsd", &full(&stsd_body));

        let stts = bx(b"stts", &full(&u32s(&[1, 1, 1000])));
        let stsc = bx(b"stsc", &full(&u32s(&[1, 1, 1, 1])));
        let stsz = bx(b"stsz", &full(&u32s(&[payload.len() as u32, 1])));

        let mut stco = vec![0u8; 20];
        stco[0..4].copy_from_slice(&20u32.to_be_bytes());
        stco[4..8].copy_from_slice(b"stco");
        stco[12..16].copy_from_slice(&1u32.to_be_bytes()); // entry count

        let stbl = bx(b"stbl", &{
            let mut body = Vec::new();
            body.extend_from_slice(&stsd);
            body.extend_from_slice(&stts);
            body.extend_from_slice(&stsc);
            body.extend_from_slice(&stsz);
            body.extend_from_slice(&stco);
            body
        });
        let minf = bx(b"minf", &{
            let mut body = Vec::new();
            body.extend_from_slice(&bx(b"vmhd", &full(&[0u8; 8])));
            body.extend_from_slice(&stbl);
            body
        });
        let mdia = bx(b"mdia", &{
            let mut body = Vec::new();
            body.extend_from_slice(&mdhd);
            body.extend_from_slice(&hdlr);
            body.extend_from_slice(&minf);
            body
        });
        let trak = bx(b"trak", &{
            let mut body = Vec::new();
            body.extend_from_slice(&tkhd);
            body.extend_from_slice(&mdia);
            body
        });

        let mut mvhd = vec![0u8; 108];
        mvhd[0..4].copy_from_slice(&108u32.to_be_bytes());
        mvhd[4..8].copy_from_slice(b"mvhd");
        mvhd[20..24].copy_from_slice(&1000u32.to_be_bytes()); // timescale
        mvhd[24..28].copy_from_slice(&1000u32.to_be_bytes()); // duration
        mvhd[104..108].copy_from_slice(&2u32.to_be_bytes()); // next track id

        let moov = bx(b"moov", &{
            let mut body = Vec::new();
            body.extend_from_slice(&mvhd);
            body.extend_from_slice(&trak);
            body
        });

        let mut file = Vec::new();
        file.extend_from_slice(&ftyp);
        file.extend_from_slice(&moov);

        // The payload begins right after this file's ftyp and moov.
        let mdat_payload_at = file.len() + 8;
        let mut moov = moov;
        // `find_bytes` locates the type field, so the box starts four bytes
        // earlier and its single entry sits sixteen bytes after that.
        let entry = find_bytes(&moov, b"stco").expect("stco in the fixture") + 12;
        moov[entry..entry + 4].copy_from_slice(&(mdat_payload_at as u32).to_be_bytes());

        let mut written = Vec::new();
        written.extend_from_slice(&ftyp);
        written.extend_from_slice(&moov);
        written.extend_from_slice(&((payload.len() as u32) + 8).to_be_bytes());
        written.extend_from_slice(b"mdat");
        written.extend_from_slice(payload);
        written
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tafas-mux-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn remuxes_two_tracks_and_keeps_their_payloads() {
        let dir = temp_dir("remux");
        let video_payload: Vec<u8> = (0..100u8).collect();
        let audio_payload = vec![0xBBu8; 50];
        let video = dir.join("v.mp4");
        let audio = dir.join("a.m4a");
        let out = dir.join("out.mp4");
        std::fs::write(&video, one_track_mp4(b"vide", &video_payload)).unwrap();
        std::fs::write(&audio, one_track_mp4(b"soun", &audio_payload)).unwrap();

        // The fixture has to point at its own payload, or the remux refuses it
        // before doing anything interesting.
        let bytes = std::fs::read(&video).unwrap();
        let stco = find_bytes(&bytes, b"stco").expect("stco in the fixture");
        let entry = be_u32(&bytes, stco + 12).unwrap() as usize;
        assert!(
            entry > stco && entry < bytes.len(),
            "stco at {stco}, entry {entry}, file {} bytes",
            bytes.len()
        );

        let report = remux(&video, &audio, &out).unwrap();
        assert_eq!(report.video_tracks, 1);
        assert_eq!(report.audio_tracks, 1);

        let verified = verify(&out).unwrap();
        let handlers: Vec<&str> = verified
            .tracks
            .iter()
            .map(|track| track.handler.as_str())
            .collect();
        assert_eq!(handlers, ["vide", "soun"]);
        assert_eq!(verified.sample_total(), 2);
        assert_eq!(verified.bytes, std::fs::metadata(&out).unwrap().len());

        let written = std::fs::read(&out).unwrap();
        assert!(
            find_bytes(&written, &video_payload).is_some(),
            "video payload lost"
        );
        assert!(
            find_bytes(&written, &audio_payload).is_some(),
            "audio payload lost"
        );
        // moov first: a player can read the index before the payload.
        assert!(find_bytes(&written, b"moov").unwrap() < find_bytes(&written, b"mdat").unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_source_is_an_error_and_leaves_no_file() {
        let dir = temp_dir("truncated");
        let video = dir.join("v.mp4");
        let audio = dir.join("a.m4a");
        let out = dir.join("out.mp4");
        let mut bytes = one_track_mp4(b"vide", &[0u8; 10]);
        bytes.truncate(bytes.len() - 4);
        std::fs::write(&video, &bytes).unwrap();
        std::fs::write(&audio, one_track_mp4(b"soun", &[0u8; 10])).unwrap();

        let error = remux(&video, &audio, &out).unwrap_err();
        assert!(!matches!(error, Error::Verify(_)), "unexpected {error:?}");
        assert!(!out.exists(), "a refused remux must not leave a file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_reports_a_missing_file_as_io() {
        let dir = temp_dir("missing");
        let error = verify(&dir.join("nope.mp4")).unwrap_err();
        assert!(matches!(error, Error::Io(_)), "unexpected {error:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scale_keeps_durations_proportional_and_bounded() {
        assert_eq!(scale(1000, 1000, 600), 600);
        assert_eq!(scale(0, 1000, 600), 0);
        assert_eq!(scale(1, 0, 600), 0);
        assert_eq!(scale(u64::MAX, 1, u32::MAX), u64::MAX);
    }

    fn box_of(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn metadata_is_emptied_without_moving_a_byte() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&box_of(b"trak", b"trak-body"));
        let udta = box_of(b"udta", b"iTunes said hello");
        let udta_len = udta.len();
        payload.extend_from_slice(&udta);
        let before = payload.clone();

        let mut removed = Vec::new();
        let mut blanked = 0u64;
        let mut zeroed = 0usize;
        strip_moov(&mut payload, &mut removed, &mut blanked, &mut zeroed).unwrap();

        assert_eq!(removed, vec!["udta".to_string()]);
        assert_eq!(payload.len(), before.len(), "the header must not shrink");
        // The box is now `free` of the identical size, and holds no trace of
        // what it used to say.
        assert_eq!(
            &payload[before.len() - udta_len + 4..before.len() - udta_len + 8],
            b"free"
        );
        assert_eq!(
            u32::from_be_bytes([
                payload[before.len() - udta_len],
                payload[before.len() - udta_len + 1],
                payload[before.len() - udta_len + 2],
                payload[before.len() - udta_len + 3],
            ]) as usize,
            udta_len
        );
        assert!(!payload.windows(4).any(|window| window == b"iTun"));
    }

    #[test]
    fn a_track_keeps_its_identity_while_losing_its_timestamp() {
        // mvhd version 0: creation and modification are the two u32 after the
        // version/flags word.
        let mut mvhd_body = vec![0u8; 100];
        mvhd_body[4..8].copy_from_slice(&1_700_000_000u32.to_be_bytes());
        mvhd_body[8..12].copy_from_slice(&1_700_000_100u32.to_be_bytes());
        let mut payload = box_of(b"mvhd", &mvhd_body);

        let mut removed = Vec::new();
        let mut blanked = 0u64;
        let mut zeroed = 0usize;
        strip_moov(&mut payload, &mut removed, &mut blanked, &mut zeroed).unwrap();

        let body = 8 + 4;
        assert_eq!(&payload[body..body + 4], &[0, 0, 0, 0]);
        assert_eq!(&payload[body + 4..body + 8], &[0, 0, 0, 0]);
        assert!(removed.is_empty(), "zeroing a time is not a removal");
        assert_eq!(zeroed, 1, "but it is still work worth reporting");
    }

    #[test]
    fn udta_inside_a_track_is_found_one_level_down() {
        let mut trak_body = box_of(b"tkhd", &[0u8; 92]);
        trak_body.extend_from_slice(&box_of(b"udta", b"recorded on a phone"));
        let mut payload = box_of(b"trak", &trak_body);

        let mut removed = Vec::new();
        let mut blanked = 0u64;
        let mut zeroed = 0usize;
        strip_moov(&mut payload, &mut removed, &mut blanked, &mut zeroed).unwrap();

        assert_eq!(removed, vec!["udta".to_string()]);
    }

    #[test]
    fn an_unknown_box_is_kept() {
        // The default has to be "keep": a box this build does not know could be
        // the one a player needs.
        let mut payload = Vec::new();
        payload.extend_from_slice(&box_of(b"mvex", b"payload"));
        let before = payload.clone();

        let mut removed = Vec::new();
        let mut blanked = 0u64;
        let mut zeroed = 0usize;
        strip_moov(&mut payload, &mut removed, &mut blanked, &mut zeroed).unwrap();

        assert!(removed.is_empty());
        assert_eq!(blanked, 0);
        assert_eq!(payload, before);
    }

    #[test]
    fn mdat_header_grows_past_four_gibibytes() {
        assert_eq!(mdat_header_len(100), 8);
        assert_eq!(mdat_header_len(u32::MAX as u64 - 8), 8);
        assert_eq!(mdat_header_len(u32::MAX as u64 - 7), 16);
    }

    #[test]
    fn an_offset_outside_every_mdat_is_refused() {
        let source = Source {
            ftyp: Vec::new(),
            moov: Vec::new(),
            mdats: vec![(100, 200)],
            len: 200,
            top: Vec::new(),
        };
        assert_eq!(map_offset(150, &source, &[1000]).unwrap(), 1050);
        assert!(matches!(
            map_offset(50, &source, &[1000]).unwrap_err(),
            Error::Refused(_)
        ));
    }
}

// ---------------------------------------------------------------------------
// --strip-meta
// ---------------------------------------------------------------------------

/// Box types that carry what a file says about *itself* rather than the media
/// in it: `udta` (a container, so everything inside it goes — `ilst`, `©nam`,
/// `©too`, the lot), `meta`, `ilst` when it appears without a parent `udta`,
/// `chpl` (chapters, which name scenes), `Xtra`, and `uuid`, whose contents are
/// a vendor's business and frequently include a device identity.
///
/// Everything not on this list is kept, including every box this module does not
/// otherwise know. That direction matters: keeping an unrecognised box is safe,
/// and dropping the wrong one could cost a player the thing that makes the file
/// play. `stsd`, `stts`, `stco` and the codec configuration boxes are never
/// touched.
const META_BOXES: [&[u8; 4]; 6] = [b"udta", b"meta", b"ilst", b"chpl", b"Xtra", b"uuid"];

fn is_metadata(kind: &[u8; 4]) -> bool {
    META_BOXES.contains(&kind)
}

/// Empties the metadata in a plain ISO-BMFF file, keeping the media byte for
/// byte and the file the same length.
///
/// The length is the point. A chunk offset is an absolute byte position, so
/// removing a box that sits before the payload it refers to would leave every
/// sample after it pointed at by an index that had quietly become wrong —
/// unless the tables were rewritten, which is what [`remux`] does and what this
/// function exists to avoid. Every box here is therefore replaced by padding of
/// its own size rather than deleted: `moov` comes out exactly as long as it went
/// in, `mdat` is copied untouched, and the index is copied rather than
/// understood. There is no layout in which that can misplace a sample.
///
/// What remains is bytes: a `free` box holds nothing, so no reader finds a title
/// or a timestamp where one used to be. The file does not get smaller; the part
/// of it that described the file rather than the media stops describing anything.
///
/// A file with no metadata is left as it is, byte for byte, rather than being
/// rewritten into an identical file: the caller can then say "nothing to strip"
/// instead of claiming work it did not do.
pub fn strip(source: &Path, out: &Path) -> Result<StripReport> {
    let scanned = scan(source).map_err(|e| add_path(e, source))?;
    let span = moov_span(&scanned);
    let bytes_before = scanned.len;

    // `moov` is copied whole and then edited in place, so its length never
    // changes and nothing after it moves. What changes is only what the boxes
    // inside it say.
    let mut payload = scanned.moov[span.payload..span.end].to_vec();
    let mut removed: Vec<String> = Vec::new();
    let mut bytes_blanked = 0u64;
    let mut zeroed = 0usize;
    strip_moov(&mut payload, &mut removed, &mut bytes_blanked, &mut zeroed)?;

    // A top-level metadata box is only blanked when it can be: an 8-byte header
    // and a 32-bit size field. A 64-bit one is left in place and not claimed.
    let blankable_top = scanned
        .top
        .iter()
        .filter(|b| {
            is_metadata(&b.kind) && b.payload - b.start == 8 && b.end - b.start <= u32::MAX as u64
        })
        .count();

    // Nothing found, nothing to write — no second file, and the caller can say
    // so. Clearing a timestamp counts as finding something, which is why this
    // looks at `zeroed` and not only at the boxes.
    if removed.is_empty() && blankable_top == 0 && zeroed == 0 {
        return Ok(StripReport {
            bytes_before,
            bytes_after: bytes_before,
            removed,
            bytes_blanked: 0,
            zeroed,
        });
    }

    let file = File::create(out)?;
    let mut writer = BufWriter::with_capacity(1 << 20, file);
    let mut reader = File::open(source)?;
    let mut buffer = vec![0u8; 1 << 20];
    for top in &scanned.top {
        match &top.kind {
            b"moov" => {
                // The length is derived from the edited payload, but it is the
                // source's own length that the file's offsets were built
                // against, so the two are checked against each other rather than
                // assumed to agree. The check at the end of the function would
                // catch a disagreement too; this one names the box.
                let size = 8 + payload.len() as u64;
                if size != top.end - top.start {
                    return Err(Error::Internal(format!(
                        "mp4: stripping changed the size of moov from {} to {size}",
                        top.end - top.start
                    )));
                }
                writer.write_all(&(size as u32).to_be_bytes())?;
                writer.write_all(b"moov")?;
                writer.write_all(&payload)?;
            }
            kind if is_metadata(kind) => {
                let size = top.end - top.start;
                if top.payload - top.start != 8 || size > u32::MAX as u64 {
                    // Not blanked, and not claimed: see `blank_in_place`.
                    copy_range(&mut reader, &mut writer, &mut buffer, top.start, top.end)?;
                    continue;
                }
                removed.push(name_of(kind));
                bytes_blanked += size;
                blank_range(&mut writer, &mut buffer, size)?;
            }
            // `mdat` lands here, and it has to: `copy_mdats` writes a header of
            // its own choosing, and an `mdat` whose header changed width would
            // move its payload — the one thing this function must never do.
            _ => copy_range(&mut reader, &mut writer, &mut buffer, top.start, top.end)?,
        }
    }
    writer.flush()?;

    let file = writer.into_inner().map_err(|e| Error::Io(e.into_error()))?;
    file.sync_all()?;
    let bytes_after = file.metadata()?.len();
    if bytes_after != bytes_before {
        return Err(Error::Internal(format!(
            "mp4: stripping changed the file length from {bytes_before} to {bytes_after}; the \
             chunk offsets would no longer hold"
        )));
    }

    Ok(StripReport {
        bytes_before,
        bytes_after,
        removed,
        bytes_blanked,
        zeroed,
    })
}

/// Empties the metadata inside a `moov` payload, in place, and reports both what
/// it emptied and how many bytes became padding.
fn strip_moov(
    payload: &mut [u8],
    removed: &mut Vec<String>,
    blanked: &mut u64,
    zeroed: &mut usize,
) -> Result<()> {
    for span in children(payload, 0, payload.len())? {
        if is_metadata(&span.kind) {
            if blank_in_place(payload, span) {
                removed.push(name_of(&span.kind));
                *blanked += (span.end - span.start) as u64;
            }
            continue;
        }
        match &span.kind {
            b"mvhd" | b"tkhd" | b"mdhd" => {
                if zero_times(payload, span)? {
                    *zeroed += 1;
                }
            }
            // Only the containers a `udta` can be hiding in. Sample tables are
            // never entered: there is nothing to find below them, and walking
            // them would mean understanding them.
            b"trak" | b"mdia" | b"minf" => {
                let mut inner = payload[span.payload..span.end].to_vec();
                let mut nested = Vec::new();
                strip_moov(&mut inner, &mut nested, blanked, zeroed)?;
                payload[span.payload..span.end].copy_from_slice(&inner);
                removed.append(&mut nested);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Writes a zeroed creation and modification time into `mvhd`, `tkhd` or `mdhd`.
///
/// A file tafas assembled already has zeros here; one that was copied from a
/// site usually does not, and those two fields are the most identifying thing a
/// media file carries — a timestamp to the second, with no frames involved. The
/// fields are fixed width, so zeroing them cannot move a byte. Durations and
/// timescales are left untouched: they are what makes the file play.
/// Returns whether either field was non-zero, so a file whose times are already
/// cleared is not reported as changed.
fn zero_times(data: &mut [u8], span: Span) -> Result<bool> {
    let version = *data
        .get(span.payload)
        .ok_or_else(|| Error::Parse("mp4: truncated time header".into()))?;
    let (first, width) = match version {
        0 => (span.payload + 4, 4),
        1 => (span.payload + 4, 8),
        v => {
            return Err(Error::Parse(format!(
                "mp4: {} version {v} is not known",
                show(&span.kind)
            )))
        }
    };
    let second = first + width;
    if second + width > span.end {
        return Err(Error::Parse(format!(
            "mp4: {} is too short for its version {version} times",
            show(&span.kind)
        )));
    }
    let touched = data[first..first + width].iter().any(|b| *b != 0)
        || data[second..second + width].iter().any(|b| *b != 0);
    data[first..first + width].fill(0);
    data[second..second + width].fill(0);
    Ok(touched)
}

/// Empties a box inside a buffer without changing its length.
///
/// Returns `false` for a box whose header is 64-bit, which cannot be replaced by
/// an 8-byte `free` header without changing what the size field means. Such a
/// box is left alone and the caller does not count it as removed: a `udta` past
/// four gigabytes is not a case to guess at, and silently producing a file whose
/// header disagrees with its length is the one outcome worth ruling out.
fn blank_in_place(data: &mut [u8], span: Span) -> bool {
    let size = span.end - span.start;
    let header = span.payload - span.start;
    if header != 8 || size > u32::MAX as usize {
        return false;
    }
    data[span.start..span.start + 4].copy_from_slice(&(size as u32).to_be_bytes());
    data[span.start + 4..span.start + 8].copy_from_slice(b"free");
    data[span.payload..span.end].fill(0);
    true
}

/// The same, for a top-level box that lives in the file rather than in memory.
fn blank_range(writer: &mut BufWriter<File>, buffer: &mut [u8], size: u64) -> Result<()> {
    let mut header = [0u8; 8];
    header[..4].copy_from_slice(&(size as u32).to_be_bytes());
    header[4..].copy_from_slice(b"free");
    writer.write_all(&header)?;
    let mut left = size - 8;
    while left > 0 {
        let want = buffer.len().min(left as usize);
        buffer[..want].fill(0);
        writer.write_all(&buffer[..want])?;
        left -= want as u64;
    }
    Ok(())
}

fn copy_range(
    reader: &mut File,
    writer: &mut BufWriter<File>,
    buffer: &mut [u8],
    start: u64,
    end: u64,
) -> Result<()> {
    reader.seek(SeekFrom::Start(start))?;
    let mut left = end - start;
    while left > 0 {
        let want = buffer.len().min(left as usize);
        reader.read_exact(&mut buffer[..want])?;
        writer.write_all(&buffer[..want])?;
        left -= want as u64;
    }
    Ok(())
}

fn name_of(kind: &[u8; 4]) -> String {
    show(kind).trim_matches('"').to_string()
}

/// What [`strip`] did, so the caller can say it rather than imply it.
#[derive(Debug, Clone)]
pub struct StripReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// The box types that were emptied, in the order they appeared.
    pub removed: Vec<String>,
    /// How many bytes of the file are now padding. The file is the same length
    /// as before; this is how much of it no longer says anything.
    pub bytes_blanked: u64,
    /// How many creation/modification timestamp pairs were zeroed.
    pub zeroed: usize,
}

impl StripReport {
    /// Whether the source had metadata at all — removed or zeroed. A `false`
    /// here is what makes `--strip-meta` on a file tafas wrote a no-op instead
    /// of a rewrite.
    pub fn changed(&self) -> bool {
        !self.removed.is_empty() || self.zeroed > 0
    }
}
