//! Writing an MP4 from demuxed tracks.
//!
//! This is the other end of [`crate::ts`]: the demuxer hands over samples in
//! order with their timestamps and a codec record, and this module lays them
//! into a plain ISO-BMFF file — `ftyp`, `moov`, one `mdat` — that a player can
//! open without a fragment index.
//!
//! Layout is chosen for one property: the `moov` comes first, so a player can
//! read the index before it has the payload. That is the same choice
//! [`crate::mux`] makes when it remuxes two streams, and it is what makes the
//! file usable while it is still being copied.
//!
//! Nothing here re-encodes. Sample payloads are written exactly as the demuxer
//! produced them; the only bytes this module invents are the boxes that
//! describe them, which is what an ISO-BMFF muxer is.
//!
//! [`write`] finishes by running [`crate::mux::verify`] over the file it just
//! wrote. That is not belt-and-braces: the sample tables are built by hand, and
//! a table that disagrees with the payload by four bytes is a file that plays
//! for a second and then stops.

use std::fs::File;
use std::io::{BufWriter, Write as IoWrite};
use std::path::Path;

use crate::error::{Error, Result};
use crate::mux;
use crate::ts::{AacConfig, Codec, Track, TrackKind};

/// What a written file contains.
#[derive(Debug, Clone)]
pub struct WriteReport {
    pub bytes: u64,
    pub video_samples: u64,
    pub audio_samples: u64,
    /// Length of the longest track, in milliseconds.
    pub duration_ms: u64,
}

/// The movie timescale. One thousand ticks a second keeps durations readable in
/// a hex dump and matches what every muxer writes.
const MOVIE_TIMESCALE: u32 = 1000;

/// Writes `tracks` to `out`.
///
/// `dimensions` is the picture size to declare when the transport stream does
/// not carry it — `(0, 0)` when nothing is known, which is what a playlist
/// without `RESOLUTION` leaves us with. Players read the real size from the SPS
/// either way; the field exists so that a thumbnail in a file manager is not
/// blank.
pub fn write(tracks: &[Track], dimensions: (u32, u32), out: &Path) -> Result<WriteReport> {
    if tracks.is_empty() {
        return Err(Error::Internal(
            "asked to write an MP4 with no tracks".into(),
        ));
    }
    for track in tracks {
        if track.samples.is_empty() {
            return Err(Error::Internal(format!(
                "the {:?} track has no samples",
                track.kind
            )));
        }
    }

    let durations: Vec<u64> = tracks.iter().map(track_duration).collect();
    let movie_duration = tracks
        .iter()
        .zip(&durations)
        .map(|(track, duration)| scale(*duration, track.timescale, MOVIE_TIMESCALE))
        .max()
        .unwrap_or(0);

    let ftyp = build_ftyp();
    // The index depends on where the payload starts, and the payload starts
    // after the index. Building the index twice, with a placeholder and then
    // with the real offset, is cheaper to read than an in-place patch and just
    // as exact: no table changes size when an offset changes.
    let placeholder = build_moov(tracks, &durations, dimensions, movie_duration, &[])?;
    let payload_at = ftyp.len() as u64 + placeholder.len() as u64 + 8;
    let offsets = track_offsets(tracks, payload_at);
    let moov = build_moov(tracks, &durations, dimensions, movie_duration, &offsets)?;
    if moov.len() != placeholder.len() {
        return Err(Error::Internal(
            "the movie header changed size between passes".into(),
        ));
    }
    if moov.len() as u64 > u32::MAX as u64 {
        return Err(Error::Limit(
            "the movie header does not fit in a 32-bit box size".into(),
        ));
    }

    let payload: u64 = tracks
        .iter()
        .flat_map(|track| track.samples.iter())
        .map(|sample| sample.data.len() as u64)
        .sum();
    if payload + 8 > u32::MAX as u64 {
        return Err(Error::Limit(
            "this recording needs a 64-bit mdat, which tafas does not write yet; split the \
             source or use --no-mux"
                .into(),
        ));
    }

    let file = File::create(out)?;
    let mut writer = BufWriter::with_capacity(1 << 20, file);
    writer.write_all(&ftyp)?;
    writer.write_all(&(moov.len() as u32).to_be_bytes())?;
    writer.write_all(b"moov")?;
    writer.write_all(&moov)?;
    writer.write_all(&((payload as u32) + 8).to_be_bytes())?;
    writer.write_all(b"mdat")?;
    for track in tracks {
        for sample in &track.samples {
            writer.write_all(&sample.data)?;
        }
    }
    writer.flush()?;
    drop(writer);

    let bytes = std::fs::metadata(out)?.len();
    // The writer checks its own work with the reader that predates it.
    let verified = mux::verify(out)?;
    let expected_video = tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .map(|track| track.samples.len() as u64)
        .sum::<u64>();
    let expected_audio = tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .map(|track| track.samples.len() as u64)
        .sum::<u64>();
    if verified.sample_total() != expected_video + expected_audio {
        return Err(Error::Verify(format!(
            "{}: wrote {} samples, the reader found {}",
            out.display(),
            expected_video + expected_audio,
            verified.sample_total()
        )));
    }
    if expected_video > 0 && !verified.has_handler("vide") {
        return Err(Error::Verify(format!(
            "{}: the video track is not in the movie header",
            out.display()
        )));
    }
    if expected_audio > 0 && !verified.has_handler("soun") {
        return Err(Error::Verify(format!(
            "{}: the audio track is not in the movie header",
            out.display()
        )));
    }

    Ok(WriteReport {
        bytes,
        video_samples: expected_video,
        audio_samples: expected_audio,
        duration_ms: movie_duration,
    })
}

/// Where each track's samples begin, once the payload is laid out track by
/// track in the order they were given.
fn track_offsets(tracks: &[Track], payload_at: u64) -> Vec<u64> {
    let mut offsets = Vec::with_capacity(tracks.len());
    let mut at = payload_at;
    for track in tracks {
        offsets.push(at);
        let bytes: u64 = track
            .samples
            .iter()
            .map(|sample| sample.data.len() as u64)
            .sum();
        at += bytes;
    }
    offsets
}

fn track_duration(track: &Track) -> u64 {
    match samples_layout(track) {
        Layout::Uniform { count, delta } => count.saturating_mul(delta),
        Layout::Timed { deltas, .. } => deltas.iter().map(|delta| u64::from(*delta)).sum(),
    }
}

fn scale(value: u64, from: u32, to: u32) -> u64 {
    if from == 0 {
        return 0;
    }
    (value as u128 * to as u128 / from as u128) as u64
}

/// How a track's samples are spaced in time.
enum Layout {
    /// A constant step: every audio frame is one AAC frame long.
    Uniform { count: u64, delta: u64 },
    /// Per-sample deltas, from the difference between consecutive decode times.
    Timed { deltas: Vec<u32>, ctts: Vec<i32> },
}

fn samples_layout(track: &Track) -> Layout {
    if track.kind == TrackKind::Audio {
        let delta = match &track.codec {
            Codec::Aac(config) => config.frame_samples as u64,
            _ => 1024,
        };
        return Layout::Uniform {
            count: track.samples.len() as u64,
            delta,
        };
    }

    let times: Vec<u64> = track
        .samples
        .iter()
        .map(|sample| sample.dts.or(sample.pts).unwrap_or(0))
        .collect();
    let mut deltas: Vec<u32> = Vec::with_capacity(times.len());
    for (index, time) in times.iter().enumerate() {
        let delta: u64 = match times.get(index + 1) {
            Some(next) if next > time => next - time,
            // The last sample, or a timestamp that did not advance: reuse the
            // previous step, which is what a player assumes anyway.
            _ => deltas
                .last()
                .map(|last| u64::from(*last))
                .unwrap_or(1)
                .max(1),
        };
        deltas.push(u32::try_from(delta).unwrap_or(u32::MAX));
    }
    let ctts: Vec<i32> = track
        .samples
        .iter()
        .map(|sample| match (sample.pts, sample.dts) {
            (Some(pts), Some(dts)) => pts.saturating_sub(dts).min(i32::MAX as u64) as i32,
            _ => 0,
        })
        .collect();
    Layout::Timed { deltas, ctts }
}

// ---------------------------------------------------------------------------
// boxes
// ---------------------------------------------------------------------------

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&((payload.len() as u32) + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn full(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(payload.len() + 4);
    body.push(version);
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(payload);
    bx(kind, &body)
}

fn build_ftyp() -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"isom");
    payload.extend_from_slice(&0x0000_0200u32.to_be_bytes());
    for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
        payload.extend_from_slice(brand);
    }
    bx(b"ftyp", &payload)
}

fn build_moov(
    tracks: &[Track],
    durations: &[u64],
    dimensions: (u32, u32),
    movie_duration: u64,
    offsets: &[u64],
) -> Result<Vec<u8>> {
    let mut payload = Vec::new();

    // mvhd
    let mut mvhd = Vec::new();
    mvhd.extend_from_slice(&0u32.to_be_bytes()); // creation time
    mvhd.extend_from_slice(&0u32.to_be_bytes()); // modification time
    mvhd.extend_from_slice(&MOVIE_TIMESCALE.to_be_bytes());
    mvhd.extend_from_slice(&(movie_duration.min(u32::MAX as u64) as u32).to_be_bytes());
    mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    mvhd.extend_from_slice(&0u16.to_be_bytes()); // reserved
    mvhd.extend_from_slice(&[0u8; 8]); // reserved
    mvhd.extend_from_slice(&identity_matrix());
    mvhd.extend_from_slice(&[0u8; 24]); // pre_defined
    let next_track_id = tracks.len() as u32 + 1;
    mvhd.extend_from_slice(&next_track_id.to_be_bytes());
    payload.extend_from_slice(&full(b"mvhd", 0, 0, &mvhd));

    for (index, track) in tracks.iter().enumerate() {
        let track_id = index as u32 + 1;
        let duration = durations[index];
        let movie_ticks = scale(duration, track.timescale, MOVIE_TIMESCALE);
        let offset = offsets.get(index).copied();
        payload.extend_from_slice(&build_trak(
            track,
            track_id,
            movie_ticks,
            dimensions,
            offset,
        )?);
    }

    Ok(payload)
}

fn identity_matrix() -> [u8; 36] {
    let mut matrix = [0u8; 36];
    matrix[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    matrix[16..20].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    matrix[32..36].copy_from_slice(&0x4000_0000u32.to_be_bytes());
    matrix
}

fn build_trak(
    track: &Track,
    track_id: u32,
    movie_ticks: u64,
    dimensions: (u32, u32),
    payload_at: Option<u64>,
) -> Result<Vec<u8>> {
    let is_video = track.kind == TrackKind::Video;

    // tkhd
    let mut tkhd = Vec::new();
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&track_id.to_be_bytes());
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // reserved
    tkhd.extend_from_slice(&(movie_ticks.min(u32::MAX as u64) as u32).to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 8]); // reserved
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // layer
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // alternate group
    let volume: u16 = if is_video { 0 } else { 0x0100 };
    tkhd.extend_from_slice(&volume.to_be_bytes());
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // reserved
    tkhd.extend_from_slice(&identity_matrix());
    let (width, height) = if is_video { dimensions } else { (0, 0) };
    tkhd.extend_from_slice(&(width << 16).to_be_bytes());
    tkhd.extend_from_slice(&(height << 16).to_be_bytes());
    let mut trak = Vec::new();
    trak.extend_from_slice(&full(b"tkhd", 0, 0x0000_0007, &tkhd));

    // mdia
    let mut mdhd = Vec::new();
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&track.timescale.to_be_bytes());
    let track_ticks = track_duration(track);
    mdhd.extend_from_slice(&(track_ticks.min(u32::MAX as u64) as u32).to_be_bytes());
    mdhd.extend_from_slice(&0x55c4u16.to_be_bytes()); // 'und'
    mdhd.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    let mut mdia = Vec::new();
    mdia.extend_from_slice(&full(b"mdhd", 0, 0, &mdhd));

    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    hdlr.extend_from_slice(if is_video { b"vide" } else { b"soun" });
    hdlr.extend_from_slice(&[0u8; 12]); // reserved
    hdlr.extend_from_slice(if is_video {
        b"VideoHandler\0"
    } else {
        b"SoundHandler\0"
    });
    mdia.extend_from_slice(&full(b"hdlr", 0, 0, &hdlr));

    // minf
    let mut minf = Vec::new();
    if is_video {
        let mut vmhd = vec![0u8; 8]; // graphicsmode + opcolor
        vmhd[0..2].copy_from_slice(&0u16.to_be_bytes());
        minf.extend_from_slice(&full(b"vmhd", 0, 1, &vmhd));
    } else {
        minf.extend_from_slice(&full(b"smhd", 0, 0, &[0u8; 4]));
    }
    minf.extend_from_slice(&build_dinf());
    minf.extend_from_slice(&build_stbl(track, payload_at)?);
    // `minf` and `mdia` are plain containers: no version, no flags. Wrapping
    // them the way a full box is wrapped shifts every box after them by four
    // bytes, which is exactly the kind of error a reader notices first.
    mdia.extend_from_slice(&bx(b"minf", &minf));

    trak.extend_from_slice(&bx(b"mdia", &mdia));
    Ok(bx(b"trak", &trak))
}

fn build_dinf() -> Vec<u8> {
    // A self-contained file points at itself: one `url` entry with a flag that
    // says "the data is in this file".
    let mut url = Vec::new();
    url.push(0u8); // version
    url.extend_from_slice(&0x0000_0001u32.to_be_bytes()[1..]); // flags: self-contained
    let url_box = bx(b"url ", &url);
    let mut dref = Vec::new();
    dref.extend_from_slice(&1u32.to_be_bytes()); // entry count
    dref.extend_from_slice(&url_box);
    let dref = full(b"dref", 0, 0, &dref);
    bx(b"dinf", &dref)
}

fn build_stbl(track: &Track, payload_at: Option<u64>) -> Result<Vec<u8>> {
    let mut stbl = Vec::new();
    stbl.extend_from_slice(&build_stsd(track)?);

    match samples_layout(track) {
        Layout::Uniform { count, delta } => {
            let mut stts = Vec::new();
            stts.extend_from_slice(&1u32.to_be_bytes()); // entry count
            stts.extend_from_slice(&(count.min(u32::MAX as u64) as u32).to_be_bytes());
            stts.extend_from_slice(&(delta.min(u32::MAX as u64) as u32).to_be_bytes());
            stbl.extend_from_slice(&full(b"stts", 0, 0, &stts));
        }
        Layout::Timed { deltas, ctts } => {
            let runs = runs(&deltas);
            let mut stts = Vec::new();
            stts.extend_from_slice(&(runs.len() as u32).to_be_bytes());
            for (count, delta) in &runs {
                stts.extend_from_slice(&(*count).to_be_bytes());
                stts.extend_from_slice(&(*delta).to_be_bytes());
            }
            stbl.extend_from_slice(&full(b"stts", 0, 0, &stts));

            if ctts.iter().any(|offset| *offset != 0) {
                let runs = signed_runs(&ctts);
                let mut body = Vec::new();
                body.extend_from_slice(&(runs.len() as u32).to_be_bytes());
                for (count, offset) in &runs {
                    body.extend_from_slice(&(*count).to_be_bytes());
                    body.extend_from_slice(&offset.to_be_bytes());
                }
                stbl.extend_from_slice(&full(b"ctts", 0, 0, &body));
            }
        }
    }

    // One chunk per track: every sample of a stream is already contiguous in
    // the payload, so a chunk table with a single entry describes it exactly.
    let mut stsc = Vec::new();
    stsc.extend_from_slice(&1u32.to_be_bytes()); // entry count
    stsc.extend_from_slice(&1u32.to_be_bytes()); // first chunk
    stsc.extend_from_slice(&(track.samples.len() as u32).to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes()); // sample description index
    stbl.extend_from_slice(&full(b"stsc", 0, 0, &stsc));

    let mut stsz = Vec::new();
    stsz.extend_from_slice(&0u32.to_be_bytes()); // sample size: per-sample
    stsz.extend_from_slice(&(track.samples.len() as u32).to_be_bytes());
    for sample in &track.samples {
        stsz.extend_from_slice(&(sample.data.len() as u32).to_be_bytes());
    }
    stbl.extend_from_slice(&full(b"stsz", 0, 0, &stsz));

    let offset = payload_at.unwrap_or(0);
    if offset > u32::MAX as u64 {
        let mut co64 = Vec::new();
        co64.extend_from_slice(&1u32.to_be_bytes());
        co64.extend_from_slice(&offset.to_be_bytes());
        stbl.extend_from_slice(&full(b"co64", 0, 0, &co64));
    } else {
        let mut stco = Vec::new();
        stco.extend_from_slice(&1u32.to_be_bytes());
        stco.extend_from_slice(&(offset as u32).to_be_bytes());
        stbl.extend_from_slice(&full(b"stco", 0, 0, &stco));
    }

    if track.kind == TrackKind::Video {
        let keyframes: Vec<u32> = track
            .samples
            .iter()
            .enumerate()
            .filter(|(_, sample)| sample.keyframe)
            .map(|(index, _)| index as u32 + 1)
            .collect();
        if !keyframes.is_empty() && keyframes.len() < track.samples.len() {
            let mut stss = Vec::new();
            stss.extend_from_slice(&(keyframes.len() as u32).to_be_bytes());
            for number in &keyframes {
                stss.extend_from_slice(&number.to_be_bytes());
            }
            stbl.extend_from_slice(&full(b"stss", 0, 0, &stss));
        }
    }

    Ok(bx(b"stbl", &stbl))
}

fn build_stsd(track: &Track) -> Result<Vec<u8>> {
    let entry = match &track.codec {
        Codec::Avc { avcc } => build_avc1(avcc),
        Codec::Aac(config) => build_mp4a(config),
    };
    let mut stsd = Vec::new();
    stsd.extend_from_slice(&1u32.to_be_bytes()); // entry count
    stsd.extend_from_slice(&entry);
    Ok(full(b"stsd", 0, 0, &stsd))
}

fn sample_entry_prefix(data_reference_index: u16) -> Vec<u8> {
    let mut out = vec![0u8; 6]; // reserved
    out.extend_from_slice(&data_reference_index.to_be_bytes());
    out
}

fn build_avc1(avcc: &[u8]) -> Vec<u8> {
    let mut body = sample_entry_prefix(1);
    body.extend_from_slice(&[0u8; 16]); // pre_defined, reserved, pre_defined[3]
    body.extend_from_slice(&0u16.to_be_bytes()); // width: the SPS is authoritative
    body.extend_from_slice(&0u16.to_be_bytes()); // height
    body.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi
    body.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes()); // reserved
    body.extend_from_slice(&1u16.to_be_bytes()); // frame count
    let mut compressor = [0u8; 32];
    compressor[0] = 4;
    compressor[1..6].copy_from_slice(b"tafas");
    body.extend_from_slice(&compressor);
    body.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
    body.extend_from_slice(&0xffffu16.to_be_bytes()); // pre_defined
    body.extend_from_slice(&bx(b"avcC", avcc));
    bx(b"avc1", &body)
}

fn build_mp4a(config: &AacConfig) -> Vec<u8> {
    let mut body = sample_entry_prefix(1);
    body.extend_from_slice(&[0u8; 8]); // reserved
    body.extend_from_slice(&(u16::from(config.channels)).to_be_bytes());
    body.extend_from_slice(&16u16.to_be_bytes()); // sample size in bits
    body.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    body.extend_from_slice(&0u16.to_be_bytes()); // reserved
    body.extend_from_slice(&(config.sample_rate << 16).to_be_bytes());
    body.extend_from_slice(&bx(b"esds", &build_esds(config)));
    bx(b"mp4a", &body)
}

fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    if body.len() < 0x80 {
        out.push(body.len() as u8);
    } else {
        out.push(0x80 | ((body.len() >> 21) as u8 & 0x7f));
        out.push(0x80 | ((body.len() >> 14) as u8 & 0x7f));
        out.push(0x80 | ((body.len() >> 7) as u8 & 0x7f));
        out.push(body.len() as u8 & 0x7f);
    }
    out.extend_from_slice(body);
    out
}

fn build_esds(config: &AacConfig) -> Vec<u8> {
    let mut decoder = vec![0x40u8]; // MPEG-4 audio
    decoder.push(0x15); // audio stream
    decoder.extend_from_slice(&[0u8; 3]); // buffer size
    decoder.extend_from_slice(&0u32.to_be_bytes()); // max bitrate: unknown
    decoder.extend_from_slice(&0u32.to_be_bytes()); // average bitrate: unknown
    decoder.extend_from_slice(&descriptor(0x05, &config.asc));

    let mut es = Vec::new();
    es.extend_from_slice(&1u16.to_be_bytes()); // ES_ID
    es.push(0); // no dependency, no URL, no OCR
    es.extend_from_slice(&descriptor(0x04, &decoder));
    es.extend_from_slice(&descriptor(0x06, &[0x02])); // pre-defined SL config

    let mut body = vec![0u8; 4]; // version and flags
    body.extend_from_slice(&descriptor(0x03, &es));
    body
}

/// Run-length encodes per-sample deltas.
fn runs(deltas: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for delta in deltas {
        match out.last_mut() {
            Some((count, value)) if value == delta => *count += 1,
            _ => out.push((1, *delta)),
        }
    }
    out
}

fn signed_runs(offsets: &[i32]) -> Vec<(u32, i32)> {
    let mut out: Vec<(u32, i32)> = Vec::new();
    for offset in offsets {
        match out.last_mut() {
            Some((count, value)) if value == offset => *count += 1,
            _ => out.push((1, *offset)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::Sample;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tafas-mp4write-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn avc_track() -> Track {
        let avcc = vec![
            1, 0x42, 0x00, 0x1e, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1e, 1, 0x00, 0x02,
            0x68, 0xce,
        ];
        Track {
            kind: TrackKind::Video,
            codec: Codec::Avc { avcc },
            timescale: 90_000,
            samples: vec![
                Sample {
                    data: vec![0xaa; 100],
                    pts: Some(0),
                    dts: Some(0),
                    keyframe: true,
                },
                Sample {
                    data: vec![0xbb; 60],
                    pts: Some(3000),
                    dts: Some(3000),
                    keyframe: false,
                },
            ],
        }
    }

    fn aac_track() -> Track {
        let config = AacConfig {
            asc: [0x12, 0x10],
            sample_rate: 44_100,
            channels: 2,
            object_type: 2,
            frame_samples: 1024,
        };
        Track {
            kind: TrackKind::Audio,
            codec: Codec::Aac(config),
            timescale: 44_100,
            samples: vec![
                Sample {
                    data: vec![0x11; 40],
                    pts: Some(0),
                    dts: Some(0),
                    keyframe: true,
                },
                Sample {
                    data: vec![0x22; 40],
                    pts: None,
                    dts: None,
                    keyframe: true,
                },
            ],
        }
    }

    #[test]
    fn writes_a_file_the_reader_agrees_with() {
        let dir = temp_dir("pair");
        let out = dir.join("out.mp4");
        let tracks = vec![avc_track(), aac_track()];
        let report = write(&tracks, (1280, 720), &out).unwrap();

        assert_eq!(report.video_samples, 2);
        assert_eq!(report.audio_samples, 2);
        assert_eq!(report.bytes, std::fs::metadata(&out).unwrap().len());
        // 6000 ticks of video at 90 kHz is 66 ms; two AAC frames are 46 ms, so
        // the movie is as long as its longest track.
        assert_eq!(report.duration_ms, 66);

        let verified = mux::verify(&out).unwrap();
        assert!(verified.has_handler("vide"));
        assert!(verified.has_handler("soun"));
        assert_eq!(verified.sample_total(), 4);
        assert_eq!(verified.bytes, report.bytes);
    }

    #[test]
    fn the_index_comes_before_the_payload() {
        let dir = temp_dir("layout");
        let out = dir.join("out.mp4");
        write(&[avc_track()], (640, 360), &out).unwrap();

        let bytes = std::fs::read(&out).unwrap();
        let moov = find(&bytes, b"moov").expect("moov");
        let mdat = find(&bytes, b"mdat").expect("mdat");
        assert!(moov < mdat, "moov at {moov}, mdat at {mdat}");
        assert_eq!(&bytes[4..8], b"ftyp");
    }

    #[test]
    fn a_video_only_file_has_no_audio_handler() {
        let dir = temp_dir("video-only");
        let out = dir.join("out.mp4");
        let report = write(&[avc_track()], (0, 0), &out).unwrap();
        assert_eq!(report.audio_samples, 0);
        let verified = mux::verify(&out).unwrap();
        assert!(verified.has_handler("vide"));
        assert!(!verified.has_handler("soun"));
    }

    #[test]
    fn deltas_run_length_encode() {
        let runs = runs(&[3000, 3000, 3001, 3001, 3001]);
        assert_eq!(runs, vec![(2, 3000), (3, 3001)]);
        assert_eq!(track_duration(&avc_track()), 6000);
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
