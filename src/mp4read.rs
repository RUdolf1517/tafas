//! Fragmented MP4: the other container an HLS segment comes in.
//!
//! An fMP4 playlist is not a stream of packets the way MPEG-TS is. It is a
//! header — an *initialisation segment* named by `EXT-X-MAP`, carrying `moov`
//! with each track's timescale and codec record — followed by media segments
//! that are each a `moof` describing samples plus an `mdat` holding their bytes.
//! Nothing here decodes anything: the module turns `moof`/`trun`/`tfhd`/`tfdt`
//! into the same [`Track`]/[`Sample`] pair the transport-stream demuxer produces,
//! so `mp4write` lays both out with the one writer and neither needs to know
//! where the samples came from.
//!
//! The parts that are refused are refused by name, because a fragment this
//! module half-understands is a file that plays wrong rather than badly: HEVC
//! and AC-3 sample entries, an `AudioSpecificConfig` that is not the two-byte
//! AAC-LC record, and sample-level encryption (`senc`, which is SAMPLE-AES).
//!
//! Everything is bounds-checked against the buffer it came in: a `trun` that
//! points past its `mdat` is a parse error naming the track, never a panic and
//! never a short read of somebody else's memory.

use crate::error::{Error, Result};
use crate::ts::{sample_rate, AacConfig, Codec, Sample, Track, TrackKind};

/// A fragment that declares more samples than this is a corrupt length, not a
/// long video: twenty-four hours of 60 fps video is five million.
const SAMPLE_LIMIT: u32 = 4_000_000;

// ---------------------------------------------------------------------------
// reading bytes
// ---------------------------------------------------------------------------

/// A cursor with the bounds check built in, so every read either produces the
/// bytes it asked for or the error that says how short the box was.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, at: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.at
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.data.len());
        let end = end.ok_or_else(|| {
            Error::Parse(format!(
                "an fMP4 box ended {} bytes before its reader wanted {count}",
                count.saturating_sub(self.remaining())
            ))
        })?;
        let slice = &self.data[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn skip(&mut self, count: usize) -> Result<()> {
        self.bytes(count).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let bytes = self.bytes(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
}

/// A 24-bit flag field: the low three bytes of a full box's version/flags word.
fn flags(reader: &mut Reader<'_>) -> Result<u32> {
    let bytes = reader.bytes(3)?;
    Ok((u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]))
}

/// One box, with both ends kept: `start` is what a `trun` data offset is
/// relative to, `body_start` is where its payload begins.
#[derive(Debug, Clone, Copy)]
struct Bx {
    kind: [u8; 4],
    start: usize,
    body_start: usize,
    end: usize,
}

fn name(kind: &[u8; 4]) -> String {
    String::from_utf8_lossy(kind).into_owned()
}

/// Walks the boxes between two offsets, whatever their nesting.
fn scan(data: &[u8], from: usize, to: usize) -> Result<Vec<Bx>> {
    if from > to || to > data.len() {
        return Err(Error::Parse(format!(
            "asked to read boxes from byte {from} to {to} of a {}-byte buffer",
            data.len()
        )));
    }
    let mut boxes = Vec::new();
    let mut at = from;
    while at < to {
        if to - at < 8 {
            return Err(Error::Parse(format!(
                "a box starts at byte {at} with {} bytes left, less than a header",
                to - at
            )));
        }
        let mut reader = Reader::new(&data[at..to]);
        let size32 = reader.u32()?;
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        let (size, header) = match size32 {
            // Zero means "to the end of the enclosing box", which is what a
            // final `mdat` in a segment uses.
            0 => ((to - at) as u64, 8u64),
            1 => {
                reader.skip(4)?;
                (reader.u64()?, 16u64)
            }
            size => (u64::from(size), 8u64),
        };
        if size < header {
            return Err(Error::Parse(format!(
                "box {} at byte {at} declares {size} bytes, less than its own header",
                name(&kind)
            )));
        }
        let end = at
            .checked_add(usize::try_from(size).unwrap_or(usize::MAX))
            .filter(|end| *end <= to)
            .ok_or_else(|| {
                Error::Parse(format!(
                    "box {} at byte {at} declares {size} bytes and runs past the {} bytes it sits in",
                    name(&kind),
                    to - at
                ))
            })?;
        boxes.push(Bx {
            kind,
            start: at,
            body_start: at + header as usize,
            end,
        });
        at = end;
    }
    Ok(boxes)
}

// ---------------------------------------------------------------------------
// the initialisation segment
// ---------------------------------------------------------------------------

/// What `moov` said: one entry per track, with the sample defaults `trex`
/// carries so a `tfhd` that omits a field has something to fall back to.
#[derive(Debug, Clone)]
pub struct Init {
    tracks: Vec<InitTrack>,
}

#[derive(Debug, Clone)]
struct InitTrack {
    id: u32,
    timescale: u32,
    kind: TrackKind,
    codec: Codec,
    default_duration: u32,
    default_size: u32,
    default_flags: u32,
}

/// Reads an `EXT-X-MAP` initialisation segment.
pub fn init(data: &[u8]) -> Result<Init> {
    let top = scan(data, 0, data.len())?;
    let moov = top
        .iter()
        .find(|bx| &bx.kind == b"moov")
        .ok_or_else(|| Error::Parse("the initialisation segment has no moov box".into()))?;
    let children = scan(data, moov.body_start, moov.end)?;

    let mut defaults: Vec<(u32, [u32; 3])> = Vec::new();
    if let Some(mvex) = children.iter().find(|bx| &bx.kind == b"mvex") {
        for trex in scan(data, mvex.body_start, mvex.end)?
            .iter()
            .filter(|bx| &bx.kind == b"trex")
        {
            let mut reader = Reader::new(&data[trex.body_start..trex.end]);
            reader.skip(4)?; // version + flags
            let id = reader.u32()?;
            reader.skip(4)?; // default_sample_description_index
            let duration = reader.u32()?;
            let size = reader.u32()?;
            let flags = reader.u32()?;
            defaults.push((id, [duration, size, flags]));
        }
    }

    let mut tracks = Vec::new();
    for trak in children.iter().filter(|bx| &bx.kind == b"trak") {
        let mut track = parse_trak(data, trak)?;
        if let Some((_, [duration, size, flags])) = defaults.iter().find(|(id, _)| *id == track.id)
        {
            track.default_duration = *duration;
            track.default_size = *size;
            track.default_flags = *flags;
        }
        if track.timescale == 0 {
            return Err(Error::Parse(format!(
                "track {} of the initialisation segment declares a zero timescale",
                track.id
            )));
        }
        tracks.push(track);
    }
    if tracks.is_empty() {
        return Err(Error::Parse(
            "the initialisation segment declares no tracks".into(),
        ));
    }
    Ok(Init { tracks })
}

fn parse_trak(data: &[u8], trak: &Bx) -> Result<InitTrack> {
    let children = scan(data, trak.body_start, trak.end)?;
    let tkhd = children
        .iter()
        .find(|bx| &bx.kind == b"tkhd")
        .ok_or_else(|| Error::Parse("a trak box has no tkhd box".into()))?;
    let mut reader = Reader::new(&data[tkhd.body_start..tkhd.end]);
    let version = reader.u8()?;
    reader.skip(3)?;
    match version {
        0 => reader.skip(8)?,  // creation + modification
        1 => reader.skip(16)?, // both 64-bit
        other => {
            return Err(Error::Parse(format!(
                "a tkhd box has version {other}, which the specification does not define"
            )))
        }
    }
    let id = reader.u32()?;

    let mdia = children
        .iter()
        .find(|bx| &bx.kind == b"mdia")
        .ok_or_else(|| Error::Parse(format!("track {id} has no mdia box")))?;
    let inside = scan(data, mdia.body_start, mdia.end)?;
    let mdhd = inside
        .iter()
        .find(|bx| &bx.kind == b"mdhd")
        .ok_or_else(|| Error::Parse(format!("track {id} has no mdhd box")))?;
    let mut reader = Reader::new(&data[mdhd.body_start..mdhd.end]);
    let version = reader.u8()?;
    reader.skip(3)?;
    match version {
        0 => reader.skip(8)?,
        1 => reader.skip(16)?,
        other => {
            return Err(Error::Parse(format!(
                "an mdhd box has version {other}, which the specification does not define"
            )))
        }
    }
    let timescale = reader.u32()?;

    let minf = inside
        .iter()
        .find(|bx| &bx.kind == b"minf")
        .ok_or_else(|| Error::Parse(format!("track {id} has no minf box")))?;
    let stbl = scan(data, minf.body_start, minf.end)?
        .into_iter()
        .find(|bx| &bx.kind == b"stbl")
        .ok_or_else(|| Error::Parse(format!("track {id} has no stbl box")))?;
    let stsd = scan(data, stbl.body_start, stbl.end)?
        .into_iter()
        .find(|bx| &bx.kind == b"stsd")
        .ok_or_else(|| {
            Error::Parse(format!(
                "track {id} has no sample description table, so its codec is unknown"
            ))
        })?;
    let (kind, codec) = parse_stsd(data, &stsd)?;

    Ok(InitTrack {
        id,
        timescale,
        kind,
        codec,
        default_duration: 0,
        default_size: 0,
        default_flags: 0,
    })
}

fn parse_stsd(data: &[u8], stsd: &Bx) -> Result<(TrackKind, Codec)> {
    let mut reader = Reader::new(&data[stsd.body_start..stsd.end]);
    reader.skip(4)?; // version + flags
    let count = reader.u32()?;
    if count == 0 {
        return Err(Error::Parse(
            "a sample description table is empty, so the track has no codec".into(),
        ));
    }
    let entries = scan(data, stsd.body_start + 8, stsd.end)?;
    let entry = entries.first().ok_or_else(|| {
        Error::Parse("a sample description table declares an entry it does not contain".into())
    })?;
    match &entry.kind {
        b"avc1" | b"avc3" => {
            // A visual sample entry has 78 bytes of fixed fields after its
            // header, and its child boxes start after them.
            let children = scan(data, entry.body_start + 78, entry.end)?;
            let avcc = children
                .iter()
                .find(|bx| &bx.kind == b"avcC")
                .ok_or_else(|| {
                    Error::Parse(format!(
                        "the {} sample entry has no avcC record, so the video cannot be described",
                        name(&entry.kind)
                    ))
                })?;
            Ok((
                TrackKind::Video,
                Codec::Avc {
                    avcc: data[avcc.body_start..avcc.end].to_vec(),
                },
            ))
        }
        b"mp4a" => {
            // An audio sample entry has 28 bytes of fixed fields after its
            // header.
            let children = scan(data, entry.body_start + 28, entry.end)?;
            let esds = children
                .iter()
                .find(|bx| &bx.kind == b"esds")
                .ok_or_else(|| {
                    Error::Parse(
                        "the mp4a sample entry has no esds box, so the audio cannot be described"
                            .into(),
                    )
                })?;
            let asc = asc_from_esds(&data[esds.body_start..esds.end])?;
            Ok((TrackKind::Audio, Codec::Aac(aac_config(&asc)?)))
        }
        b"hvc1" | b"hev1" => Err(Error::Refused(
            "the fragments carry HEVC video (hvc1/hev1), which this build does not write".into(),
        )),
        b"ac-3" | b"ec-3" => Err(Error::Refused(
            "the fragments carry AC-3 audio (ac-3/ec-3), which this build does not write".into(),
        )),
        other => Err(Error::Refused(format!(
            "the fragments use the {} sample entry, which this build does not write",
            name(other)
        ))),
    }
}

/// Walks the descriptors inside an `esds` box to the decoder-specific info,
/// which for AAC is the `AudioSpecificConfig`.
fn asc_from_esds(body: &[u8]) -> Result<Vec<u8>> {
    let mut reader = Reader::new(body);
    reader.skip(4)?; // version + flags
    let (tag, body) = descriptor(&mut reader)?;
    if tag != 0x03 {
        return Err(Error::Parse(format!(
            "an esds box starts with descriptor {tag:#04x}, not an ES_Descriptor (0x03)"
        )));
    }
    let mut es = Reader::new(body);
    es.skip(2)?; // ES_ID
    let es_flags = es.u8()?;
    if es_flags & 0x80 != 0 {
        es.skip(2)?; // dependsOn_ES_ID
    }
    if es_flags & 0x40 != 0 {
        let length = usize::from(es.u8()?);
        es.skip(length)?; // URL, length-prefixed
    }
    if es_flags & 0x20 != 0 {
        es.skip(2)?; // OCR_ES_Id
    }
    let (tag, decoder) = descriptor(&mut es)?;
    if tag != 0x04 {
        return Err(Error::Parse(format!(
            "an ES_Descriptor holds descriptor {tag:#04x}, not a DecoderConfigDescriptor (0x04)"
        )));
    }
    let mut config = Reader::new(decoder);
    // objectTypeIndication, streamType, bufferSizeDB, maxBitrate, avgBitrate.
    config.skip(13)?;
    let (tag, specific) = descriptor(&mut config)?;
    if tag != 0x05 {
        return Err(Error::Parse(format!(
            "a DecoderConfigDescriptor holds descriptor {tag:#04x}, not a DecoderSpecificInfo (0x05)"
        )));
    }
    Ok(specific.to_vec())
}

/// One MPEG-4 descriptor: a tag, a length written in seven-bit groups, and the
/// payload it covers.
fn descriptor<'a>(reader: &mut Reader<'a>) -> Result<(u8, &'a [u8])> {
    let tag = reader.u8()?;
    let mut length = 0usize;
    for _ in 0..4 {
        let byte = reader.u8()?;
        length = (length << 7) | usize::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            break;
        }
    }
    Ok((tag, reader.bytes(length)?))
}

/// The two-byte `AudioSpecificConfig` an `esds` box carries, in the same shape
/// the transport-stream demuxer builds from an ADTS header.
fn aac_config(asc: &[u8]) -> Result<AacConfig> {
    if asc.len() != 2 {
        return Err(Error::Refused(format!(
            "the audio sample entry carries a {}-byte AudioSpecificConfig; this build writes \
             the two-byte AAC-LC record HLS uses",
            asc.len()
        )));
    }
    let object_type = asc[0] >> 3;
    let frequency = ((asc[0] & 0x07) << 1) | (asc[1] >> 7);
    let channels = (asc[1] >> 3) & 0x0f;
    if object_type != 2 {
        return Err(Error::Refused(format!(
            "the audio is AAC object type {object_type}; this build writes AAC-LC (2), the only \
             one HLS uses"
        )));
    }
    let rate = sample_rate(frequency).ok_or_else(|| {
        Error::Parse(format!(
            "the AudioSpecificConfig names sampling frequency index {frequency}, which is not one \
             of the thirteen rates"
        ))
    })?;
    if channels == 0 {
        return Err(Error::Parse(
            "the AudioSpecificConfig declares zero channels".into(),
        ));
    }
    Ok(AacConfig {
        asc: [asc[0], asc[1]],
        sample_rate: rate,
        channels,
        object_type,
        frame_samples: 1024,
    })
}

// ---------------------------------------------------------------------------
// fragments
// ---------------------------------------------------------------------------

/// Reads the initialisation segment and every fragment after it, and returns
/// the tracks `mp4write` needs.
pub fn read(init_bytes: &[u8], media: &[u8]) -> Result<Vec<Track>> {
    let init = init(init_bytes)?;
    fragments(&init, media)
}

/// Reads media fragments against an initialisation segment already parsed.
pub fn fragments(init: &Init, media: &[u8]) -> Result<Vec<Track>> {
    let mut states: Vec<State<'_>> = init
        .tracks
        .iter()
        .map(|track| State {
            track,
            samples: Vec::new(),
            next_dts: 0,
        })
        .collect();

    let mut fragment_count = 0usize;
    for moof in scan(media, 0, media.len())?
        .iter()
        .filter(|bx| &bx.kind == b"moof")
    {
        fragment_count += 1;
        let mut previous_end: Option<usize> = None;
        for traf in scan(media, moof.body_start, moof.end)?
            .iter()
            .filter(|bx| &bx.kind == b"traf")
        {
            let parts = scan(media, traf.body_start, traf.end)?;
            if let Some(encrypted) = parts
                .iter()
                .find(|bx| matches!(&bx.kind, b"senc" | b"saiz" | b"saio"))
            {
                return Err(Error::Refused(format!(
                    "the fragments carry sample-level encryption ({}), which is SAMPLE-AES and \
                     not undone by this build (docs/hls.md §4)",
                    name(&encrypted.kind)
                )));
            }
            let tfhd = parts
                .iter()
                .find(|bx| &bx.kind == b"tfhd")
                .ok_or_else(|| Error::Parse("a traf box has no tfhd box".into()))?;
            let header = parse_tfhd(media, tfhd)?;
            let index = states
                .iter()
                .position(|state| state.track.id == header.track_id)
                .ok_or_else(|| {
                    Error::Parse(format!(
                        "a fragment names track {} and the initialisation segment does not",
                        header.track_id
                    ))
                })?;
            let defaults = Defaults {
                duration: header
                    .duration
                    .unwrap_or(states[index].track.default_duration),
                size: header.size.unwrap_or(states[index].track.default_size),
                flags: header
                    .sample_flags
                    .unwrap_or(states[index].track.default_flags),
            };
            // Where the samples live: an explicit base, the start of the moof
            // (`default-base-is-moof`), or — for a second traf with neither —
            // the end of the data the previous one covered.
            let base = match header.base_offset {
                Some(offset) => offset,
                None if header.base_is_moof => moof.start as u64,
                None => previous_end.map_or(moof.start as u64, |end| end as u64),
            };
            let base = usize::try_from(base).map_err(|_| {
                Error::Parse("a fragment's base data offset is past this machine's memory".into())
            })?;
            let mut dts = match parts.iter().find(|bx| &bx.kind == b"tfdt") {
                Some(tfdt) => parse_tfdt(media, tfdt)?,
                None => states[index].next_dts,
            };

            let mut cursor = base;
            for trun in parts.iter().filter(|bx| &bx.kind == b"trun") {
                let run = parse_trun(media, trun, &defaults)?;
                let mut at = match run.data_offset {
                    Some(offset) => {
                        let at = i64::try_from(base)
                            .unwrap_or(i64::MAX)
                            .saturating_add(i64::from(offset));
                        usize::try_from(at).map_err(|_| {
                            Error::Parse(format!(
                                "a trun of track {} points {} bytes before its fragment",
                                header.track_id, offset
                            ))
                        })?
                    }
                    None => cursor,
                };
                for sample in &run.samples {
                    let end = at
                        .checked_add(sample.size as usize)
                        .filter(|end| *end <= media.len())
                        .ok_or_else(|| {
                            Error::Parse(format!(
                                "a sample of track {} needs {} bytes at {at} and the fragments are \
                                 {} bytes",
                                header.track_id,
                                sample.size,
                                media.len()
                            ))
                        })?;
                    let pts = i64::try_from(dts)
                        .unwrap_or(i64::MAX)
                        .saturating_add(i64::from(sample.composition_offset))
                        .max(0) as u64;
                    states[index].samples.push(Sample {
                        data: media[at..end].to_vec(),
                        pts: Some(pts),
                        dts: Some(dts),
                        keyframe: sample.keyframe,
                    });
                    at = end;
                    dts += u64::from(sample.duration);
                }
                cursor = at;
            }
            states[index].next_dts = dts;
            previous_end = Some(cursor);
        }
    }

    let mut tracks = Vec::new();
    for state in states {
        if state.samples.is_empty() {
            continue;
        }
        tracks.push(Track {
            kind: state.track.kind,
            codec: state.track.codec.clone(),
            timescale: state.track.timescale,
            samples: state.samples,
        });
    }
    if tracks.is_empty() {
        return Err(Error::Parse(format!(
            "the {fragment_count} media fragments carry no samples"
        )));
    }
    Ok(tracks)
}

struct State<'a> {
    track: &'a InitTrack,
    samples: Vec<Sample>,
    /// Where the next fragment's decode times start when it carries no `tfdt`.
    next_dts: u64,
}

/// The sample defaults in force for one `traf`: `tfhd` first, then `trex`.
#[derive(Debug, Clone, Copy)]
struct Defaults {
    duration: u32,
    size: u32,
    flags: u32,
}

#[derive(Debug, Clone, Copy)]
struct TrackHeader {
    track_id: u32,
    base_offset: Option<u64>,
    base_is_moof: bool,
    duration: Option<u32>,
    size: Option<u32>,
    sample_flags: Option<u32>,
}

fn parse_tfhd(data: &[u8], tfhd: &Bx) -> Result<TrackHeader> {
    let mut reader = Reader::new(&data[tfhd.body_start..tfhd.end]);
    reader.skip(1)?; // version
    let flags = flags(&mut reader)?;
    let track_id = reader.u32()?;
    let base_offset = if flags & 0x000001 != 0 {
        Some(reader.u64()?)
    } else {
        None
    };
    if flags & 0x000002 != 0 {
        reader.skip(4)?; // sample_description_index
    }
    let duration = if flags & 0x000008 != 0 {
        Some(reader.u32()?)
    } else {
        None
    };
    let size = if flags & 0x000010 != 0 {
        Some(reader.u32()?)
    } else {
        None
    };
    let sample_flags = if flags & 0x000020 != 0 {
        Some(reader.u32()?)
    } else {
        None
    };
    Ok(TrackHeader {
        track_id,
        base_offset,
        base_is_moof: flags & 0x020000 != 0,
        duration,
        size,
        sample_flags,
    })
}

fn parse_tfdt(data: &[u8], tfdt: &Bx) -> Result<u64> {
    let mut reader = Reader::new(&data[tfdt.body_start..tfdt.end]);
    let version = reader.u8()?;
    reader.skip(3)?;
    match version {
        0 => Ok(u64::from(reader.u32()?)),
        1 => reader.u64(),
        other => Err(Error::Parse(format!(
            "a tfdt box has version {other}, which the specification does not define"
        ))),
    }
}

#[derive(Debug, Clone, Copy)]
struct RunSample {
    size: u32,
    duration: u32,
    keyframe: bool,
    composition_offset: i32,
}

#[derive(Debug)]
struct Run {
    data_offset: Option<i32>,
    samples: Vec<RunSample>,
}

fn parse_trun(data: &[u8], trun: &Bx, defaults: &Defaults) -> Result<Run> {
    let mut reader = Reader::new(&data[trun.body_start..trun.end]);
    reader.skip(1)?; // version
    let flags = flags(&mut reader)?;
    let count = reader.u32()?;
    if count > SAMPLE_LIMIT {
        return Err(Error::Limit(format!(
            "a trun box declares {count} samples, past this build's limit of {SAMPLE_LIMIT}"
        )));
    }
    let data_offset = if flags & 0x000001 != 0 {
        Some(reader.u32()? as i32)
    } else {
        None
    };
    let first_flags = if flags & 0x000004 != 0 {
        Some(reader.u32()?)
    } else {
        None
    };
    let mut samples = Vec::with_capacity(count as usize);
    for index in 0..count {
        let duration = if flags & 0x000100 != 0 {
            reader.u32()?
        } else {
            defaults.duration
        };
        let size = if flags & 0x000200 != 0 {
            reader.u32()?
        } else {
            defaults.size
        };
        let sample_flags = if flags & 0x000400 != 0 {
            reader.u32()?
        } else if index == 0 {
            first_flags.unwrap_or(defaults.flags)
        } else {
            defaults.flags
        };
        let composition_offset = if flags & 0x000800 != 0 {
            reader.u32()? as i32
        } else {
            0
        };
        samples.push(RunSample {
            size,
            duration,
            // `sample_is_non_sync_sample` is bit 16: a clear bit is a keyframe.
            keyframe: sample_flags & 0x0001_0000 == 0,
            composition_offset,
        });
    }
    Ok(Run {
        data_offset,
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- a fixture builder -------------------------------------------------
    //
    // The fixtures are built here rather than checked in as bytes so that what
    // each test reads is visible next to what it asserts: a fragment is four
    // fields, not a 400-line hex dump.

    fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 8);
        out.extend_from_slice(&((payload.len() as u32) + 8).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn full(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(version);
        body.extend_from_slice(&flags.to_be_bytes()[1..]);
        body.extend_from_slice(payload);
        bx(kind, &body)
    }

    fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in parts {
            out.extend_from_slice(part);
        }
        out
    }

    fn tkhd(id: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // creation
        body.extend_from_slice(&0u32.to_be_bytes()); // modification
        body.extend_from_slice(&id.to_be_bytes());
        body.extend_from_slice(&[0u8; 60]); // the rest of the box, unread here
        full(b"tkhd", 0, 3, &body)
    }

    fn mdhd(timescale: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // creation
        body.extend_from_slice(&0u32.to_be_bytes()); // modification
        body.extend_from_slice(&timescale.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes()); // duration
        body.extend_from_slice(&[0u8; 4]); // language + pre_defined
        full(b"mdhd", 0, 0, &body)
    }

    /// An `avc1` sample entry: the 78 fixed bytes a visual entry has, then the
    /// child boxes.
    fn avc1(avcc: &[u8]) -> Vec<u8> {
        let mut entry = vec![0u8; 6]; // reserved
        entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        entry.extend_from_slice(&[0u8; 70]); // width, height, resolution, …
        entry.extend_from_slice(&bx(b"avcC", avcc));
        bx(b"avc1", &entry)
    }

    /// An `mp4a` sample entry: 28 fixed bytes, then `esds`.
    fn mp4a(asc: &[u8]) -> Vec<u8> {
        let mut entry = vec![0u8; 6]; // reserved
        entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        entry.extend_from_slice(&[0u8; 20]); // version, channels, rate, …
        let mut decoder_info = vec![0u8; 13];
        decoder_info.extend_from_slice(&descriptor(0x05, asc));
        let mut es = 1u16.to_be_bytes().to_vec(); // ES_ID
        es.push(0); // no flags, so no optional fields
        es.extend_from_slice(&descriptor(0x04, &decoder_info));
        let esds = full(b"esds", 0, 0, &descriptor(0x03, &es));
        entry.extend_from_slice(&esds);
        bx(b"mp4a", &entry)
    }

    /// An MPEG-4 descriptor: a tag and a one-byte length, since every fixture
    /// here is short.
    fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![tag, payload.len() as u8];
        out.extend_from_slice(payload);
        out
    }

    fn moov(id: u32, timescale: u32, sample_entry: Vec<u8>, trex_duration: u32) -> Vec<u8> {
        let stsd = full(b"stsd", 0, 0, &{
            let mut body = 1u32.to_be_bytes().to_vec();
            body.extend_from_slice(&sample_entry);
            body
        });
        let stbl = bx(b"stbl", &stsd);
        let minf = bx(b"minf", &stbl);
        let mdia = bx(b"mdia", &concat(&[mdhd(timescale), minf]));
        let trak = bx(b"trak", &concat(&[tkhd(id), mdia]));
        let mut trex_body = id.to_be_bytes().to_vec();
        trex_body.extend_from_slice(&1u32.to_be_bytes()); // sample description index
        trex_body.extend_from_slice(&trex_duration.to_be_bytes());
        trex_body.extend_from_slice(&0u32.to_be_bytes()); // sample size
        trex_body.extend_from_slice(&0u32.to_be_bytes()); // sample flags
        let trex = full(b"trex", 0, 0, &trex_body);
        let mvex = bx(b"mvex", &trex);
        let mvhd = full(b"mvhd", 0, 0, &[0u8; 96]);
        bx(b"moov", &concat(&[mvhd, trak, mvex]))
    }

    /// One `moof`+`mdat` pair holding `samples` of `(size, keyframe)`.
    ///
    /// The data offset is written relative to the moof start, so a fragment is
    /// correct wherever it sits in a buffer — which is what lets a test
    /// concatenate two of them the way a playlist does.
    fn fragment(id: u32, base_time: u64, samples: &[(u32, bool)], payload: &[u8]) -> Vec<u8> {
        let moof = |data_offset: i32| {
            let tfhd = full(b"tfhd", 0, 0x020000, &id.to_be_bytes()); // default-base-is-moof
            let tfdt = full(b"tfdt", 1, 0, &base_time.to_be_bytes());
            let mut trun_body = samples.len().to_be_bytes().to_vec();
            trun_body.extend_from_slice(&data_offset.to_be_bytes());
            for (size, keyframe) in samples {
                trun_body.extend_from_slice(&size.to_be_bytes());
                let flags: u32 = if *keyframe { 0x0200_0000 } else { 0x0001_0000 };
                trun_body.extend_from_slice(&flags.to_be_bytes());
            }
            // data offset, per-sample size and per-sample flags.
            let trun = full(b"trun", 0, 0x000601, &trun_body);
            let mfhd = full(b"mfhd", 0, 0, &1u32.to_be_bytes());
            let traf = bx(b"traf", &concat(&[tfhd, tfdt, trun]));
            bx(b"moof", &concat(&[mfhd, traf]))
        };
        let header = moof(0);
        let offset = (header.len() + 8) as i32;
        concat(&[moof(offset), bx(b"mdat", payload)])
    }

    // --- tests -------------------------------------------------------------

    #[test]
    fn a_fragment_becomes_samples_with_their_times() {
        let init = moov(1, 90_000, avc1(&[0x01, 0x64, 0x00, 0x1f]), 3_000);
        let media = fragment(1, 0, &[(4, true), (4, false)], b"AAAABBBB");
        let tracks = read(&init, &media).unwrap();
        assert_eq!(tracks.len(), 1);
        let track = &tracks[0];
        assert_eq!(track.kind, TrackKind::Video);
        assert_eq!(track.timescale, 90_000);
        assert_eq!(track.samples.len(), 2);
        assert_eq!(track.samples[0].data, b"AAAA");
        assert_eq!(track.samples[1].data, b"BBBB");
        assert_eq!(track.samples[0].dts, Some(0));
        assert_eq!(track.samples[1].dts, Some(3_000));
        assert!(track.samples[0].keyframe);
        assert!(!track.samples[1].keyframe);
    }

    #[test]
    fn decode_times_come_from_tfdt_and_carry_across_fragments() {
        // Each fragment says where it starts, which is what makes them
        // independent of the ones before them.
        let init = moov(1, 90_000, avc1(&[0x01, 0x64, 0x00, 0x1f]), 3_000);
        let media = concat(&[
            fragment(1, 0, &[(4, true)], b"AAAA"),
            fragment(1, 3_000, &[(4, false)], b"BBBB"),
        ]);
        let tracks = read(&init, &media).unwrap();
        let times: Vec<Option<u64>> = tracks[0].samples.iter().map(|sample| sample.dts).collect();
        assert_eq!(times, vec![Some(0), Some(3_000)]);
    }

    #[test]
    fn an_aac_track_is_described_from_its_esds() {
        // AAC-LC, 44.1 kHz, stereo: the two bytes every HLS audio rendition uses.
        let init = moov(2, 44_100, mp4a(&[0x12, 0x10]), 1_024);
        let media = fragment(2, 0, &[(5, true), (5, true)], b"aaaaabbbbb");
        let tracks = read(&init, &media).unwrap();
        let track = &tracks[0];
        assert_eq!(track.kind, TrackKind::Audio);
        match &track.codec {
            Codec::Aac(config) => {
                assert_eq!(config.sample_rate, 44_100);
                assert_eq!(config.channels, 2);
                assert_eq!(config.object_type, 2);
                assert_eq!(config.asc, [0x12, 0x10]);
            }
            other => panic!("expected AAC, got {other:?}"),
        }
    }

    #[test]
    fn an_hevc_sample_entry_is_refused_by_name() {
        let mut entry = vec![0u8; 78];
        entry.extend_from_slice(&bx(b"hvcC", &[0u8; 4]));
        let bytes = bx(
            b"moov",
            &bx(
                b"trak",
                &concat(&[
                    tkhd(1),
                    bx(
                        b"mdia",
                        &concat(&[
                            mdhd(90_000),
                            bx(
                                b"minf",
                                &bx(
                                    b"stbl",
                                    &full(b"stsd", 0, 0, &{
                                        let mut body = 1u32.to_be_bytes().to_vec();
                                        body.extend_from_slice(&bx(b"hvc1", &entry));
                                        body
                                    }),
                                ),
                            ),
                        ]),
                    ),
                ]),
            ),
        );
        let error = super::init(&bytes).unwrap_err();
        assert!(error.to_string().contains("HEVC"), "{error}");
    }

    #[test]
    fn a_sample_that_runs_past_the_buffer_is_a_parse_error() {
        let init = moov(1, 90_000, avc1(&[0x01, 0x64, 0x00, 0x1f]), 3_000);
        let media = fragment(1, 0, &[(4, true), (4, false)], b"AAAA"); // too short
        let error = read(&init, &media).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("track 1"), "{text}");
        assert!(text.contains("bytes"), "{text}");
    }

    #[test]
    fn the_fixture_has_the_boxes_the_reader_walks() {
        let media = fragment(1, 0, &[(4, true), (4, false)], b"AAAABBBB");
        let top = scan(&media, 0, media.len()).unwrap();
        let kinds: Vec<String> = top
            .iter()
            .map(|bx| format!("{}({}..{})", name(&bx.kind), bx.start, bx.end))
            .map(|k| k.to_string())
            .collect();
        let moof = top.into_iter().find(|bx| &bx.kind == b"moof").unwrap();
        let inside: Vec<String> = scan(&media, moof.body_start, moof.end)
            .unwrap()
            .iter()
            .map(|bx| format!("{}({}..{})", name(&bx.kind), bx.body_start, bx.end))
            .collect();
        let traf = scan(&media, moof.body_start, moof.end)
            .unwrap()
            .into_iter()
            .find(|bx| &bx.kind == b"traf")
            .unwrap();
        let parts: Vec<String> = scan(&media, traf.body_start, traf.end)
            .unwrap()
            .iter()
            .map(|bx| format!("{}:{:02x?}", name(&bx.kind), &media[bx.body_start..bx.end]))
            .collect();
        let trun = scan(&media, traf.body_start, traf.end)
            .unwrap()
            .into_iter()
            .find(|bx| &bx.kind == b"trun")
            .unwrap();
        let run = parse_trun(
            &media,
            &trun,
            &Defaults {
                duration: 3_000,
                size: 0,
                flags: 0,
            },
        );
        panic!("top {kinds:?} inside {inside:?} parts {parts:?} run {run:?}");
    }

    #[test]
    fn an_initialisation_segment_without_moov_is_refused() {
        let error = init(&bx(b"ftyp", b"isom\x00\x00\x02\x00")).unwrap_err();
        assert!(error.to_string().contains("moov"), "{error}");
    }

    #[test]
    fn a_fragment_naming_an_unknown_track_is_refused() {
        let init = moov(1, 90_000, avc1(&[0x01, 0x64, 0x00, 0x1f]), 3_000);
        let media = fragment(9, 0, &[(4, true)], b"AAAA");
        let error = read(&init, &media).unwrap_err();
        assert!(error.to_string().contains("track 9"), "{error}");
    }
}
