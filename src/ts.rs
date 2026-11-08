//! MPEG-TS: what an HLS segment actually is.
//!
//! A transport stream is a stream of 188-byte packets, and everything this
//! module does is put them back together: find the program table, find the
//! streams it names, glue each stream's payload into access units, and describe
//! the result in the terms an MP4 writer needs — timescale, samples, keyframes,
//! and the codec initialisation a player cannot guess.
//!
//! Two conversions happen here rather than in the writer, because they are
//! facts about the *transport* layer:
//!
//! * H.264 arrives in Annex B framing (`00 00 01` start codes, SPS and PPS
//!   inline) and leaves as AVCC (four-byte lengths, SPS and PPS hoisted into an
//!   `avcC` record);
//! * AAC arrives wrapped in ADTS headers, one per frame, and leaves as raw
//!   frames plus the `AudioSpecificConfig` an `esds` box carries.
//!
//! Streams this build cannot describe are refused by name — HEVC, MPEG audio,
//! AC-3 — because a track whose codec record is guessed at is a file that
//! fails to play rather than a file that plays badly.

use crate::error::{Error, Result};

/// One transport packet, sync byte included.
pub const PACKET: usize = 188;
const SYNC: u8 = 0x47;
/// Timestamps in a transport stream count 90 kHz ticks.
pub const CLOCK: u32 = 90_000;

/// What the demuxer found.
#[derive(Debug, Clone)]
pub struct Demuxed {
    pub tracks: Vec<Track>,
    /// The clock every video timestamp is expressed in.
    pub timescale: u32,
}

impl Demuxed {
    pub fn video(&self) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
    }

    pub fn audio(&self) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    /// Timed text. Nothing in a transport stream produces one; it is what
    /// `--embed-subs` adds to a file the writer already made.
    Subtitle,
}

#[derive(Debug, Clone)]
pub enum Codec {
    /// `avcC`, the record an `avc1` sample entry carries.
    Avc { avcc: Vec<u8> },
    /// `AudioSpecificConfig`, the record an `esds`/`mp4a` sample entry carries.
    Aac(AacConfig),
    /// WebVTT in ISO-BMFF: a `wvtt` sample entry, one `vttc` box per sample.
    Wvtt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    pub asc: [u8; 2],
    pub sample_rate: u32,
    pub channels: u8,
    /// AAC object type: 2 is AAC-LC, which is what every HLS segment uses.
    pub object_type: u8,
    /// Samples a frame decodes to: 1024 for AAC-LC.
    pub frame_samples: u32,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub kind: TrackKind,
    pub codec: Codec,
    pub timescale: u32,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub data: Vec<u8>,
    /// Presentation time in the track's timescale.
    pub pts: Option<u64>,
    /// Decode time in the track's timescale. Equal to `pts` when the stream has
    /// no reordering.
    pub dts: Option<u64>,
    pub keyframe: bool,
    /// How long the sample lasts, in the track's timescale, when it is not
    /// simply the distance to the next decode time. A subtitle cue is exactly
    /// that case: cues have gaps between them, and a gap is not a duration.
    pub duration: Option<u32>,
}

// ---------------------------------------------------------------------------
// demuxing
// ---------------------------------------------------------------------------

/// Reads every packet in `data` and returns the tracks it describes.
///
/// `data` is one or more whole segments concatenated: transport streams are
/// packet-aligned, so a segment boundary is never mid-packet, and timestamps
/// continue across it.
pub fn demux(data: &[u8]) -> Result<Demuxed> {
    if data.len() < PACKET {
        return Err(Error::Parse(format!(
            "transport stream is {} bytes, less than one packet",
            data.len()
        )));
    }
    if data.len() % PACKET != 0 {
        return Err(Error::Parse(format!(
            "transport stream is {} bytes, which is not a whole number of {} byte packets",
            data.len(),
            PACKET
        )));
    }

    let mut state = State::default();
    for (index, packet) in data.chunks_exact(PACKET).enumerate() {
        state.packet(packet, index)?;
    }
    state.finish()
}

#[derive(Default)]
struct State {
    pmt_pid: Option<u16>,
    /// PID → the stream number it carries, in the order the PMT listed it.
    streams: Vec<(u16, u8)>,
    pending: Vec<Pending>,
    seen_program: bool,
}

struct Pending {
    pid: u16,
    stream_type: u8,
    buffer: Vec<u8>,
    open: bool,
    samples: Vec<RawSample>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    aac: Option<AacConfig>,
}

struct RawSample {
    /// Annex B (video) or ADTS-wrapped (audio) payload, before conversion.
    payload: Vec<u8>,
    pts: Option<u64>,
    dts: Option<u64>,
}

fn read_timestamp(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 5 {
        return None;
    }
    if bytes[0] & 0x01 == 0 {
        return None;
    }
    let high = ((bytes[0] as u64 >> 1) & 0x07) << 30;
    let mid = ((bytes[1] as u64) << 22) | (((bytes[2] as u64) >> 1) << 15);
    let low = ((bytes[3] as u64) << 7) | ((bytes[4] as u64) >> 1);
    Some(high | mid | low)
}

impl State {
    fn packet(&mut self, packet: &[u8], index: usize) -> Result<()> {
        if packet[0] != SYNC {
            return Err(Error::Parse(format!(
                "transport stream lost sync at packet {index}: first byte is {:#04x}, not 0x47",
                packet[0]
            )));
        }
        let pid = (((packet[1] & 0x1f) as u16) << 8) | packet[2] as u16;
        let unit_start = packet[1] & 0x40 != 0;
        let adaptation = packet[3] & 0x30;

        // 0b01 is a reserved value; a packet carrying it has no usable payload.
        let mut offset = 4usize;
        match adaptation {
            0x30 => {
                let length = packet[4] as usize;
                offset = 5 + length;
            }
            // Adaptation field only, or the reserved value: no payload to read.
            0x20 | 0x00 => return Ok(()),
            _ => {}
        }
        if offset >= PACKET {
            return Ok(());
        }
        let payload = &packet[offset..];

        if pid == 0 {
            self.program_table(payload);
        } else if Some(pid) == self.pmt_pid {
            self.program_map(payload);
        } else if let Some(position) = self.streams.iter().position(|(stream, _)| *stream == pid) {
            let stream_type = self.streams[position].1;
            self.elementary(pid, stream_type, payload, unit_start);
        }
        Ok(())
    }

    fn pending_for(&mut self, pid: u16, stream_type: u8) -> &mut Pending {
        if let Some(position) = self.pending.iter().position(|pending| pending.pid == pid) {
            return &mut self.pending[position];
        }
        self.pending.push(Pending {
            pid,
            stream_type,
            buffer: Vec::new(),
            open: false,
            samples: Vec::new(),
            sps: None,
            pps: None,
            aac: None,
        });
        self.pending.last_mut().expect("just pushed")
    }

    fn program_table(&mut self, payload: &[u8]) {
        let section = match payload.split_first() {
            // The first byte is a pointer to the section, which is zero in
            // practice; a non-zero pointer means stuffing this build does not
            // need to read.
            Some((0, rest)) => rest,
            _ => return,
        };
        if section.len() < 12 || section[0] != 0x00 {
            return;
        }
        let entries = &section[8..];
        for entry in entries.chunks_exact(4) {
            let program = u16::from_be_bytes([entry[0], entry[1]]);
            let pid = (((entry[2] & 0x1f) as u16) << 8) | entry[3] as u16;
            if program != 0 {
                self.pmt_pid = Some(pid);
                self.seen_program = true;
                return;
            }
        }
    }

    fn program_map(&mut self, payload: &[u8]) {
        let section = match payload.split_first() {
            Some((0, rest)) => rest,
            _ => return,
        };
        if section.len() < 12 || section[0] != 0x02 {
            return;
        }
        let info_length = (((section[10] & 0x0f) as usize) << 8) | section[11] as usize;
        let mut at = 12 + info_length;
        while at + 5 <= section.len() {
            let stream_type = section[at];
            let pid = (((section[at + 1] & 0x1f) as u16) << 8) | section[at + 2] as u16;
            let es_length = (((section[at + 3] & 0x0f) as usize) << 8) | section[at + 4] as usize;
            if !self.streams.iter().any(|(existing, _)| *existing == pid) {
                self.streams.push((pid, stream_type));
            }
            at += 5 + es_length;
        }
    }

    fn elementary(&mut self, pid: u16, stream_type: u8, payload: &[u8], unit_start: bool) {
        let pending = self.pending_for(pid, stream_type);
        if unit_start {
            if pending.open && !pending.buffer.is_empty() {
                let buffer = std::mem::take(&mut pending.buffer);
                pending.samples.push(split_pes(&buffer));
            }
            pending.buffer.clear();
            pending.open = true;
        }
        if pending.open {
            pending.buffer.extend_from_slice(payload);
        }
    }

    fn finish(mut self) -> Result<Demuxed> {
        if !self.seen_program {
            return Err(Error::Parse(
                "transport stream has no program table: this is not a segment of a multiplexed \
                 stream tafas can read"
                    .into(),
            ));
        }
        if self.streams.is_empty() {
            return Err(Error::Parse(
                "transport stream's program table names no streams".into(),
            ));
        }

        // The last packet of each stream never sees a following unit start, so
        // its payload is flushed here.
        let mut pending = std::mem::take(&mut self.pending);
        for stream in &mut pending {
            if stream.open && !stream.buffer.is_empty() {
                let buffer = std::mem::take(&mut stream.buffer);
                stream.samples.push(split_pes(&buffer));
            }
        }

        let mut tracks = Vec::new();
        for stream in pending {
            match stream.stream_type {
                0x1b => {
                    if let Some(track) = avc_track(stream)? {
                        tracks.push(track);
                    }
                }
                0x0f => {
                    if let Some(track) = aac_track(stream)? {
                        tracks.push(track);
                    }
                }
                0x24 => {
                    return Err(Error::Unsupported(
                        "this segment carries HEVC video; tafas writes H.264 and AAC only, and \
                         guessing at an hvcC record would produce a file that does not play"
                            .into(),
                    ))
                }
                0x03 | 0x04 => {
                    return Err(Error::Unsupported(
                        "this segment carries MPEG audio (MP3); tafas writes AAC only".into(),
                    ))
                }
                0x81 | 0x87 => {
                    return Err(Error::Unsupported(
                        "this segment carries AC-3 audio; tafas writes AAC only".into(),
                    ))
                }
                // Subtitles, ID3 metadata and anything else the program table
                // lists are skipped: they are not part of the video file.
                _ => {}
            }
        }

        if tracks.is_empty() {
            return Err(Error::Unsupported(
                "this segment carries no stream tafas can write: no H.264 video and no AAC audio \
                 were found"
                    .into(),
            ));
        }
        Ok(Demuxed {
            tracks,
            timescale: CLOCK,
        })
    }
}

/// Splits one PES packet into its payload and its timestamps.
///
/// The length field is ignored on purpose: a video PES is allowed to declare
/// zero (unbounded), and the packet boundary has already told us where it ends.
fn split_pes(buffer: &[u8]) -> RawSample {
    let empty = RawSample {
        payload: Vec::new(),
        pts: None,
        dts: None,
    };
    if buffer.len() < 9 || buffer[0] != 0x00 || buffer[1] != 0x00 || buffer[2] != 0x01 {
        return empty;
    }
    let flags = buffer[7] >> 6;
    let header_length = buffer[8] as usize;
    let pts = if flags >= 2 {
        buffer.get(9..).and_then(read_timestamp)
    } else {
        None
    };
    let dts = if flags == 3 {
        buffer.get(14..).and_then(read_timestamp)
    } else {
        pts
    };
    let start = 9 + header_length;
    let payload = if start <= buffer.len() {
        buffer[start..].to_vec()
    } else {
        Vec::new()
    };
    RawSample { payload, pts, dts }
}

fn avc_track(mut stream: Pending) -> Result<Option<Track>> {
    let mut samples = Vec::new();
    for raw in std::mem::take(&mut stream.samples) {
        let mut out = Vec::with_capacity(raw.payload.len());
        let mut keyframe = false;
        for unit in annex_b_units(&raw.payload) {
            if unit.is_empty() {
                continue;
            }
            match unit[0] & 0x1f {
                // Access unit delimiter, SPS, PPS and filler are not samples;
                // SPS and PPS are hoisted into the avcC record below.
                7 => {
                    // Hoisted into the avcC record, not repeated in the sample.
                    stream.sps.get_or_insert_with(|| unit.to_vec());
                    continue;
                }
                8 => {
                    stream.pps.get_or_insert_with(|| unit.to_vec());
                    continue;
                }
                9 | 12 => continue,
                5 => keyframe = true,
                _ => {}
            }
            if unit.len() > u32::MAX as usize {
                return Err(Error::Limit(
                    "an H.264 NAL unit is larger than 4 GiB".into(),
                ));
            }
            out.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            out.extend_from_slice(unit);
        }
        if out.is_empty() {
            continue;
        }
        samples.push(Sample {
            duration: None,
            data: out,
            pts: raw.pts,
            dts: raw.dts,
            keyframe,
        });
    }

    if samples.is_empty() {
        return Ok(None);
    }
    let sps = stream.sps.ok_or_else(|| {
        Error::Parse(
            "the video stream never carried an SPS/PPS pair, so there is no avcC record to write \
             and no player could decode the result"
                .into(),
        )
    })?;
    let pps = stream
        .pps
        .ok_or_else(|| Error::Parse("the video stream carried an SPS but no PPS".into()))?;
    let avcc = build_avcc(&sps, &pps)?;
    Ok(Some(Track {
        kind: TrackKind::Video,
        codec: Codec::Avc { avcc },
        timescale: CLOCK,
        samples,
    }))
}

fn aac_track(mut stream: Pending) -> Result<Option<Track>> {
    let mut samples = Vec::new();
    for raw in std::mem::take(&mut stream.samples) {
        let mut at = 0usize;
        while at + 7 <= raw.payload.len() {
            let header = &raw.payload[at..];
            if header[0] != 0xff || header[1] & 0xf0 != 0xf0 {
                // Not an ADTS frame: skip a byte and resynchronise rather than
                // writing a sample that starts mid-header.
                at += 1;
                continue;
            }
            let protection_absent = header[1] & 0x01 == 1;
            let profile = (header[2] >> 6) & 0x03;
            let frequency = (header[2] >> 2) & 0x0f;
            let channels = ((header[2] & 0x01) << 2) | ((header[3] & 0xc0) >> 6);
            let length = (((header[3] & 0x03) as usize) << 11)
                | ((header[4] as usize) << 3)
                | ((header[5] as usize) >> 5);
            if length < 7 || at + length > raw.payload.len() {
                return Err(Error::Parse(
                    "an ADTS frame declares a length that runs past its PES packet".into(),
                ));
            }
            let header_length = if protection_absent { 7 } else { 9 };
            if length <= header_length {
                at += length;
                continue;
            }
            if stream.aac.is_none() {
                stream.aac = Some(aac_config(profile, frequency, channels)?);
            }
            samples.push(Sample {
                duration: None,
                data: raw.payload[at + header_length..at + length].to_vec(),
                // Only the first frame of a PES carries a timestamp; the rest
                // follow at a fixed frame length, which the writer derives from
                // the track's timescale and `frame_samples`.
                pts: if samples.is_empty() { raw.pts } else { None },
                dts: if samples.is_empty() { raw.dts } else { None },
                keyframe: true,
            });
            at += length;
        }
    }

    if samples.is_empty() {
        return Ok(None);
    }
    let config = stream
        .aac
        .ok_or_else(|| Error::Parse("the audio stream carried no complete ADTS frame".into()))?;
    Ok(Some(Track {
        kind: TrackKind::Audio,
        codec: Codec::Aac(config),
        timescale: config.sample_rate,
        samples,
    }))
}

const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// The sampling frequency a four-bit index names.
///
/// Shared with the fragmented-MP4 reader, which meets the same table through an
/// `AudioSpecificConfig` instead of an ADTS header: the thirteen rates are a
/// property of AAC, not of the transport.
pub fn sample_rate(frequency_index: u8) -> Option<u32> {
    SAMPLE_RATES.get(frequency_index as usize).copied()
}

fn aac_config(profile: u8, frequency_index: u8, channels: u8) -> Result<AacConfig> {
    let sample_rate = sample_rate(frequency_index).ok_or_else(|| {
        Error::Parse(format!(
            "ADTS sampling frequency index {frequency_index} is not one of the thirteen rates"
        ))
    })?;
    if channels == 0 {
        return Err(Error::Parse("ADTS frame declares zero channels".into()));
    }
    // AudioSpecificConfig: 5 bits of object type, 4 of frequency index, 4 of
    // channel configuration, then three zero flags. Thirteen bits, two bytes.
    let object_type = profile + 1;
    let mut asc = [0u8; 2];
    asc[0] = (object_type << 3) | (frequency_index >> 1);
    asc[1] = ((frequency_index & 0x01) << 7) | (channels << 3);
    Ok(AacConfig {
        asc,
        sample_rate,
        channels,
        object_type,
        frame_samples: 1024,
    })
}

/// Splits Annex B byte-stream framing into NAL units, without their start codes.
fn annex_b_units(payload: &[u8]) -> Vec<&[u8]> {
    let mut units: Vec<&[u8]> = Vec::new();
    let mut start: Option<usize> = None;
    let mut at = 0usize;
    while at + 3 <= payload.len() {
        if payload[at] == 0 && payload[at + 1] == 0 && payload[at + 2] == 1 {
            if let Some(begin) = start {
                units.push(&payload[begin..at]);
            }
            at += 3;
            start = Some(at);
            continue;
        }
        if at + 4 <= payload.len()
            && payload[at] == 0
            && payload[at + 1] == 0
            && payload[at + 2] == 0
            && payload[at + 3] == 1
        {
            if let Some(begin) = start {
                units.push(&payload[begin..at]);
            }
            at += 4;
            start = Some(at);
            continue;
        }
        at += 1;
    }
    if let Some(begin) = start {
        if begin < payload.len() {
            units.push(&payload[begin..]);
        }
    }
    units
}

/// Builds the `avcC` record: profile, compatibility, level, and the one SPS and
/// one PPS a stream this build accepts carries.
fn build_avcc(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>> {
    if sps.len() < 4 {
        return Err(Error::Parse(
            "the SPS is too short to describe a profile, a level and a configuration".into(),
        ));
    }
    if sps.len() > u16::MAX as usize || pps.len() > u16::MAX as usize {
        return Err(Error::Limit(
            "an SPS or PPS does not fit in an avcC record".into(),
        ));
    }
    let mut avcc = Vec::with_capacity(11 + sps.len() + pps.len());
    avcc.push(1); // configurationVersion
    avcc.extend_from_slice(&sps[1..4]); // AVCProfileIndication, compatibility, level
    avcc.push(0xff); // 6 reserved bits, then lengthSizeMinusOne = 3
    avcc.push(0xe1); // 3 reserved bits, then numOfSequenceParameterSets = 1
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(sps);
    avcc.push(1); // numOfPictureParameterSets
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(pps);
    Ok(avcc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(pid: u16, unit_start: bool, counter: u8, payload: &[u8]) -> Vec<u8> {
        assert!(
            payload.len() <= PACKET - 4,
            "a packet holds 184 payload bytes"
        );
        let mut out = Vec::with_capacity(PACKET);
        out.push(SYNC);
        out.push((if unit_start { 0x40 } else { 0x00 }) | ((pid >> 8) as u8 & 0x1f));
        out.push(pid as u8);
        let stuffing = PACKET - 4 - payload.len();
        if stuffing == 0 {
            out.push(0x10 | (counter & 0x0f)); // payload only
        } else {
            out.push(0x30 | (counter & 0x0f)); // adaptation field and payload
            let field_length = stuffing - 1;
            out.push(field_length as u8);
            if field_length > 0 {
                out.push(0x00); // flags: nothing to signal
                out.resize(out.len() + field_length - 1, 0xff);
            }
        }
        out.extend_from_slice(payload);
        assert_eq!(out.len(), PACKET);
        out
    }

    fn pat(program_map_pid: u16) -> Vec<u8> {
        // table_id, section_length, transport_stream_id, version, section
        // number, last section number: eight bytes before the entries.
        let mut section = vec![0u8; 8];
        section[0] = 0x00; // table_id: program association
        section[1] = 0xb0; // section_syntax_indicator and the high length bits
        section[2] = 0x0d; // section_length: 13 bytes follow
        let mut entry = [0u8; 4];
        entry[0..2].copy_from_slice(&1u16.to_be_bytes());
        entry[2] = 0xe0 | ((program_map_pid >> 8) as u8 & 0x1f);
        entry[3] = program_map_pid as u8;
        section.extend_from_slice(&entry);
        // pointer_field, then the section
        let mut payload = vec![0u8];
        payload.extend_from_slice(&section);
        payload
    }

    fn pmt(video_pid: u16, audio_pid: u16) -> Vec<u8> {
        let mut section = vec![0u8; 12];
        section[0] = 0x02; // table_id: program map
        section[1] = 0xb0;
        section[2] = 0x17; // section_length
                           // program_info_length is zero, bytes 10 and 11
        section.push(0x1b); // H.264
        section.push(0xe0 | ((video_pid >> 8) as u8 & 0x1f));
        section.push(video_pid as u8);
        section.push(0xf0); // es_info_length high bits
        section.push(0x00);
        section.push(0x0f); // AAC
        section.push(0xe0 | ((audio_pid >> 8) as u8 & 0x1f));
        section.push(audio_pid as u8);
        section.push(0xf0);
        section.push(0x00);
        let mut payload = vec![0u8];
        payload.extend_from_slice(&section);
        payload
    }

    fn timestamp_bytes(prefix: u8, value: u64) -> [u8; 5] {
        [
            prefix | (((value >> 30) as u8 & 0x07) << 1) | 0x01,
            ((value >> 22) & 0xff) as u8,
            (((value >> 15) & 0x7f) as u8) << 1 | 0x01,
            ((value >> 7) & 0xff) as u8,
            ((value & 0x7f) as u8) << 1 | 0x01,
        ]
    }

    fn pes(stream_id: u8, pts: u64, dts: Option<u64>, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x00, 0x00, 0x01, stream_id];
        out.extend_from_slice(&0u16.to_be_bytes()); // unbounded length
        out.push(0x80); // marker, no scrambling
        out.push(if dts.is_some() { 0xc0 } else { 0x80 });
        out.push(if dts.is_some() { 10 } else { 5 });
        out.extend_from_slice(&timestamp_bytes(
            if dts.is_some() { 0x30 } else { 0x20 },
            pts,
        ));
        if let Some(dts) = dts {
            out.extend_from_slice(&timestamp_bytes(0x10, dts));
        }
        out.extend_from_slice(payload);
        out
    }

    const SPS: [u8; 7] = [0x67, 0x42, 0x00, 0x1e, 0x95, 0xa8, 0x14];
    const PPS: [u8; 4] = [0x68, 0xce, 0x3c, 0x80];
    const IDR: [u8; 5] = [0x65, 0x88, 0x84, 0x00, 0x0a];

    /// One packet's worth of H.264: a parameter set pair and an IDR slice.
    fn video_unit() -> Vec<u8> {
        annex_b(&[&SPS, &PPS, &IDR])
    }

    fn annex_b(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in units {
            out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
            out.extend_from_slice(unit);
        }
        out
    }

    fn adts_frame(object_type: u8, frequency_index: u8, channels: u8, payload: &[u8]) -> Vec<u8> {
        let length = payload.len() + 7;
        let mut out = vec![0u8; 7];
        out[0] = 0xff;
        out[1] = 0xf1; // MPEG-4, layer 0, no CRC
        out[2] = ((object_type - 1) << 6) | (frequency_index << 2) | ((channels >> 2) & 0x01);
        out[3] = ((channels & 0x03) << 6) | ((length >> 11) as u8 & 0x03);
        out[4] = (length >> 3) as u8;
        out[5] = (((length & 0x07) as u8) << 5) | 0x1f;
        out[6] = 0xfc;
        out.extend_from_slice(payload);
        out
    }

    fn stream(video: &[u8], audio: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&packet(0, true, 0, &pat(4096)));
        out.extend_from_slice(&packet(4096, true, 0, &pmt(481, 482)));
        out.extend_from_slice(&packet(481, true, 0, video));
        out.extend_from_slice(&packet(482, true, 0, audio));
        out
    }

    #[test]
    fn reads_a_video_and_audio_pair() {
        let video = pes(0xe0, 90_000, Some(90_000), &video_unit());
        let audio = pes(
            0xc0,
            90_000,
            None,
            &adts_frame(2, 4, 2, &[0x21, 0x10, 0x05]),
        );

        let demuxed = demux(&stream(&video, &audio)).unwrap();
        assert_eq!(demuxed.tracks.len(), 2);
        assert_eq!(demuxed.timescale, CLOCK);

        let video = demuxed.video().unwrap();
        assert_eq!(video.timescale, CLOCK);
        assert_eq!(video.samples.len(), 1);
        assert!(video.samples[0].keyframe, "an IDR slice is a keyframe");
        assert_eq!(video.samples[0].pts, Some(90_000));
        // The sample holds only the slice, length-prefixed, with no start code.
        assert_eq!(
            &video.samples[0].data[..4],
            &(IDR.len() as u32).to_be_bytes()
        );
        assert_eq!(&video.samples[0].data[4..], &IDR);

        let Codec::Avc { avcc } = &video.codec else {
            panic!("H.264 produces an avcC record");
        };
        assert_eq!(avcc[0], 1);
        assert_eq!(&avcc[1..4], &[0x42, 0x00, 0x1e], "profile, compat, level");
        assert_eq!(avcc[4], 0xff, "four-byte NAL lengths");
        assert_eq!(avcc[5], 0xe1, "one SPS");
        assert_eq!(u16::from_be_bytes([avcc[6], avcc[7]]) as usize, SPS.len());
        assert_eq!(&avcc[8..8 + SPS.len()], &SPS, "the SPS is hoisted");

        let audio = demuxed.audio().unwrap();
        assert_eq!(audio.timescale, 44_100);
        assert_eq!(audio.samples.len(), 1);
        assert_eq!(
            audio.samples[0].data,
            vec![0x21, 0x10, 0x05],
            "ADTS stripped"
        );
        let Codec::Aac(config) = &audio.codec else {
            panic!("AAC produces an AudioSpecificConfig");
        };
        assert_eq!(config.object_type, 2);
        assert_eq!(config.sample_rate, 44_100);
        assert_eq!(config.channels, 2);
        // AAC-LC, 44100 Hz, stereo, in two bytes.
        assert_eq!(config.asc, [0x12, 0x10]);
    }

    #[test]
    fn every_adts_frame_in_a_packet_is_a_sample() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&adts_frame(2, 4, 2, &[0x01, 0x02]));
        payload.extend_from_slice(&adts_frame(2, 4, 2, &[0x03, 0x04]));
        let stream = stream(
            &pes(0xe0, 0, None, &video_unit()),
            &pes(0xc0, 0, None, &payload),
        );
        let demuxed = match demux(&stream) {
            Ok(demuxed) => demuxed,
            Err(error) => panic!("{error}"),
        };
        let audio = demuxed.audio().unwrap();
        assert_eq!(audio.samples.len(), 2);
        assert_eq!(audio.samples[0].data, vec![0x01, 0x02]);
        assert_eq!(audio.samples[1].data, vec![0x03, 0x04]);
    }

    #[test]
    fn a_stream_that_is_not_a_whole_number_of_packets_is_refused() {
        let mut data = stream(
            &pes(0xe0, 0, None, &video_unit()),
            &pes(0xc0, 0, None, &adts_frame(2, 4, 2, &[0x01])),
        );
        data.truncate(data.len() - 1);
        let error = demux(&data).unwrap_err();
        assert!(error.is_parse(), "{error}");
    }

    #[test]
    fn a_stream_without_a_program_table_is_refused() {
        let data = packet(481, true, 0, &pes(0xe0, 0, None, &video_unit()));
        let error = demux(&data).unwrap_err();
        assert!(
            error.is_parse() || matches!(error, Error::Unsupported(_)),
            "{error}"
        );
    }

    #[test]
    fn hevc_is_refused_by_name() {
        let mut out = Vec::new();
        out.extend_from_slice(&packet(0, true, 0, &pat(4096)));
        let mut section = vec![0u8; 12];
        section[0] = 0x02;
        section[1] = 0xb0;
        section[2] = 0x10;
        section.push(0x24); // HEVC
        let hevc_pid: u16 = 481;
        section.push(0xe0 | ((hevc_pid >> 8) as u8 & 0x1f));
        section.push(hevc_pid as u8);
        section.push(0xf0);
        section.push(0x00);
        let mut payload = vec![0u8];
        payload.extend_from_slice(&section);
        out.extend_from_slice(&packet(4096, true, 0, &payload));
        out.extend_from_slice(&packet(481, true, 0, &pes(0xe0, 0, None, &[0x00, 0x01])));

        let error = demux(&out).unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)), "{error}");
        assert!(error.to_string().contains("HEVC"), "{error}");
    }

    #[test]
    fn annex_b_framing_is_split_on_both_start_code_lengths() {
        let payload = [
            0x00, 0x00, 0x01, 0x67, 0xaa, 0x00, 0x00, 0x00, 0x01, 0x68, 0xbb, 0x00, 0x00, 0x01,
            0x65, 0xcc,
        ];
        let units = annex_b_units(&payload);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0], &[0x67, 0xaa]);
        assert_eq!(units[1], &[0x68, 0xbb]);
        assert_eq!(units[2], &[0x65, 0xcc]);
    }

    #[test]
    fn timestamps_survive_the_five_byte_encoding() {
        let value = 0x1_2345_6789u64 & 0x7fffffff;
        assert_eq!(read_timestamp(&timestamp_bytes(0x30, value)), Some(value));
    }
}
