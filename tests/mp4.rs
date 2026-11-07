//! The test suite's own MP4 reader, tested against a file it builds itself.
//!
//! An independent verifier is only worth something if it is independent *and*
//! correct. The trees below are assembled byte by byte from the ISO-BMFF
//! definition — no crate, no `tafas` code — so a disagreement with
//! `tafas::mux::verify` is a real disagreement and not two readers sharing a
//! mistake.

mod common;

use common::mp4;

/// An ordinary box: size, type, payload.
fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&((payload.len() as u32) + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A full box: version and flags come first.
fn full(kind: &[u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![version, 0, 0, 0];
    body.extend_from_slice(payload);
    bx(kind, &body)
}

/// Boxes, back to back.
fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// Big-endian numbers, back to back.
fn join(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn be(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

fn ftyp() -> Vec<u8> {
    bx(b"ftyp", b"isom\x00\x00\x02\x00isomiso2")
}

/// A one-track file with two samples, whose single chunk starts at
/// `chunk_offset`. Sizes are 4 and 6, so `mdat` carries ten bytes.
fn one_track_file(chunk_offset: u32) -> Vec<u8> {
    // movie header: creation, modification, timescale 1000, duration 40
    let mvhd = full(b"mvhd", 0, &join(&[&be(0), &be(0), &be(1000), &be(40)]));
    // media header: timescale 90 000, duration 3000
    let mdhd = full(b"mdhd", 0, &join(&[&be(0), &be(0), &be(90_000), &be(3000)]));

    let mut hdlr_payload = vec![0u8; 4];
    hdlr_payload.extend_from_slice(b"vide");
    hdlr_payload.extend_from_slice(&[0u8; 12]);
    let hdlr = full(b"hdlr", 0, &hdlr_payload);

    // one sample description, and its format is what the codec is
    let mut stsd_payload = be(1).to_vec();
    stsd_payload.extend_from_slice(&bx(b"avc1", &[0u8; 78]));
    let stsd = full(b"stsd", 0, &stsd_payload);

    // time-to-sample: two samples of 20 ticks each
    let stts = full(b"stts", 0, &join(&[&be(1), &be(2), &be(20)]));
    // sample-to-chunk: one entry, both samples in one chunk, description 1
    let stsc = full(b"stsc", 0, &join(&[&be(1), &be(1), &be(2), &be(1)]));
    // sample sizes are explicit rather than uniform
    let stsz = full(b"stsz", 0, &join(&[&be(0), &be(2), &be(4), &be(6)]));
    let stco = full(b"stco", 0, &join(&[&be(1), &be(chunk_offset)]));

    let stbl = bx(b"stbl", &cat(&[stsd, stts, stsc, stsz, stco]));
    let minf = bx(b"minf", &stbl);
    let mdia = bx(b"mdia", &cat(&[mdhd, hdlr, minf]));
    let trak = bx(b"trak", &mdia);
    let moov = bx(b"moov", &cat(&[mvhd, trak]));

    let mdat = bx(b"mdat", &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    cat(&[ftyp(), moov, mdat])
}

/// The same file with the chunk offset pointing where the writer would have put
/// it: the first byte of `mdat`'s payload.
fn correct_file() -> Vec<u8> {
    let probe = one_track_file(0);
    let offset = probe.len() as u32 - 10;
    one_track_file(offset)
}

#[test]
fn finds_the_tracks_a_file_describes() {
    let bytes = correct_file();
    let movie = mp4::read(&bytes).expect("the file is well formed");

    assert_eq!(movie.timescale, 1000, "the movie timescale");
    assert_eq!(movie.duration, 40);
    assert_eq!(movie.tracks.len(), 1);

    let track = movie.track("vide").expect("a video track");
    assert_eq!(track.codec, "avc1");
    assert_eq!(track.timescale, 90_000);
    assert_eq!(track.samples.len(), 2);
    assert_eq!(
        track
            .samples
            .iter()
            .map(|sample| sample.size)
            .collect::<Vec<_>>(),
        vec![4, 6]
    );
    assert_eq!(
        track.keyframes(),
        2,
        "no stss box means every sample is a sync sample"
    );
}

#[test]
fn every_sample_points_inside_the_mdat() {
    let bytes = correct_file();
    let movie = mp4::read(&bytes).expect("the file is well formed");
    assert_eq!(movie.mdats.len(), 1);

    let track = movie.track("vide").expect("a video track");
    for sample in &track.samples {
        assert!(
            movie.inside_mdat(sample),
            "a sample at {} with {} bytes is outside every mdat",
            sample.offset,
            sample.size
        );
    }
    // The samples are laid out back to back, so the second starts where the
    // first ends.
    assert_eq!(track.samples[1].offset, track.samples[0].offset + 4);
}

#[test]
fn a_chunk_offset_into_the_header_is_reported() {
    let bytes = one_track_file(4);
    let movie = mp4::read(&bytes).expect("the boxes themselves are fine");
    let track = movie.track("vide").expect("a video track");
    assert!(
        !movie.inside_mdat(&track.samples[0]),
        "an offset of 4 points into the file header, not at a payload"
    );
}

#[test]
fn a_truncated_box_is_an_error_not_a_guess() {
    let mut bytes = correct_file();
    bytes.truncate(bytes.len() - 4);
    let error = mp4::read(&bytes).expect_err("the boxes no longer add up");
    assert!(
        error.contains("declares"),
        "the error should quote what the box claimed: {error}"
    );
}

#[test]
fn a_file_without_a_moov_has_no_tracks_to_find() {
    let mdat = bx(b"mdat", &[0u8; 8]);
    let error = mp4::read(&cat(&[ftyp(), mdat])).expect_err("there is no index");
    assert!(error.contains("moov"), "{error}");
}
