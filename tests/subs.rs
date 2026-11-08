//! Captions over a socket: the writer, the refusals, and the file names.
//!
//! The parser and the selection rules are unit-tested in `src/subtitles.rs`.
//! What is tested here is the part that talks to a server and to the disk: a
//! body that is written where it should be, and the two bodies that must not
//! become files at all.

mod common;

use common::mp4;
use common::{temp_dir, Route, TestServer};
use tafas::http::Http;
use tafas::model::Subtitle;
use tafas::subtitles;

fn client() -> Http {
    Http::new(1, 5, 0).expect("client")
}

fn subtitle(url: &str, language: &str, automatic: bool) -> Subtitle {
    Subtitle {
        language: language.to_string(),
        name: None,
        automatic,
        url: url.to_string(),
        headers: Vec::new(),
    }
}

const CUES: &str = "WEBVTT\n\n\
                    00:00:00.000 --> 00:00:02.000\n\
                    hello\n\n\
                    00:00:02.000 --> 00:00:04.000\n\
                    world\n";

#[test]
fn a_webvtt_body_lands_beside_the_media_file() {
    let server = TestServer::start(vec![Route::ok("/cap.vtt", CUES.as_bytes())]);
    let target = temp_dir("subs-write").join("Clip [abc].mp4");

    let written = subtitles::write(
        &client(),
        &subtitle(&server.url("/cap.vtt"), "en", false),
        &target,
    )
    .expect("a WebVTT body is written");

    assert_eq!(
        written.file_name().unwrap().to_string_lossy(),
        "Clip [abc].en.vtt"
    );
    assert_eq!(
        std::fs::read_to_string(&written).expect("the file is on disk"),
        CUES,
        "the cues are copied byte for byte"
    );
    assert!(
        !target.exists(),
        "writing captions never creates the media file"
    );
}

#[test]
fn a_machine_transcript_says_so_in_its_name() {
    let server = TestServer::start(vec![Route::ok("/cap.vtt", CUES.as_bytes())]);
    let target = temp_dir("subs-auto").join("clip.mp4");

    let written = subtitles::write(
        &client(),
        &subtitle(&server.url("/cap.vtt"), "ru", true),
        &target,
    )
    .expect("a transcript is still captions");

    assert_eq!(
        written.file_name().unwrap().to_string_lossy(),
        "clip.ru.auto.vtt",
        "a file a person wrote and a file a machine guessed do not share a name"
    );
}

#[test]
fn an_empty_body_is_refused_and_leaves_no_file() {
    let server = TestServer::start(vec![Route::ok("/cap.vtt", b"")]);
    let target = temp_dir("subs-empty").join("clip.mp4");

    let error = subtitles::write(
        &client(),
        &subtitle(&server.url("/cap.vtt"), "en", false),
        &target,
    )
    .expect_err("an empty timedtext body is a network matter, not a missing language");
    let text = error.to_string();
    assert!(text.contains("empty body"), "{text}");

    let expected = subtitles::path(&target, &subtitle("x", "en", false));
    assert!(!expected.exists(), "a refusal writes nothing");
    assert!(
        !expected.with_extension("vtt.part").exists(),
        "and leaves no temporary behind"
    );
}

#[test]
fn a_body_that_is_not_webvtt_is_refused() {
    let server = TestServer::start(vec![Route::ok("/cap.vtt", b"<transcript></transcript>")]);
    let target = temp_dir("subs-xml").join("clip.mp4");

    let error = subtitles::write(
        &client(),
        &subtitle(&server.url("/cap.vtt"), "en", false),
        &target,
    )
    .expect_err("XML is not WebVTT");
    assert!(error.to_string().contains("not WebVTT"), "{error}");
    assert!(!subtitles::path(&target, &subtitle("x", "en", false)).exists());
}

// ---------------------------------------------------------------------------
// the same cues, inside the container
// ---------------------------------------------------------------------------

fn avc_track() -> tafas::ts::Track {
    // The same shape `mp4write`'s own tests use: two frames, one keyframe.
    let avcc = vec![
        1, 0x42, 0x00, 0x1e, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1e, 1, 0x00, 0x02, 0x68,
        0xce,
    ];
    tafas::ts::Track {
        kind: tafas::ts::TrackKind::Video,
        codec: tafas::ts::Codec::Avc { avcc },
        timescale: 90_000,
        samples: vec![
            tafas::ts::Sample {
                duration: None,
                data: vec![0xaa; 100],
                pts: Some(0),
                dts: Some(0),
                keyframe: true,
            },
            tafas::ts::Sample {
                duration: None,
                data: vec![0xbb; 60],
                pts: Some(3000),
                dts: Some(3000),
                keyframe: false,
            },
        ],
    }
}

fn aac_track() -> tafas::ts::Track {
    let config = tafas::ts::AacConfig {
        asc: [0x12, 0x10],
        sample_rate: 44_100,
        channels: 2,
        object_type: 2,
        frame_samples: 1024,
    };
    tafas::ts::Track {
        kind: tafas::ts::TrackKind::Audio,
        codec: tafas::ts::Codec::Aac(config),
        timescale: 44_100,
        samples: vec![tafas::ts::Sample {
            duration: None,
            data: vec![0x11; 40],
            pts: Some(0),
            dts: Some(0),
            keyframe: true,
        }],
    }
}

/// `--embed-subs`, from the outside: a file the writer made, read back, given a
/// caption track and written again. Nothing is re-encoded, so the samples the
/// reader finds afterwards must be the ones the first write laid down.
#[test]
fn a_written_file_takes_a_caption_track_and_keeps_its_samples() {
    let dir = temp_dir("subs-embed");
    let plain = dir.join("clip.mp4");
    let first = tafas::mp4write::write(&[avc_track(), aac_track()], (640, 360), &plain)
        .expect("the first write lands");
    let before = std::fs::read(&plain).expect("the file is on disk");

    let cues = subtitles::cues(CUES.as_bytes()).expect("the fixture parses");
    assert_eq!(cues.len(), 2);

    let dimensions = tafas::mp4read::dimensions(&before).expect("the file states its size");
    assert_eq!(dimensions, (640, 360), "a rewrite keeps the picture size");

    let mut tracks = tafas::mp4read::plain(&before).expect("a written file reads back");
    assert_eq!(tracks.len(), 2, "picture and sound");
    tracks.push(subtitles::track(&cues));

    let subbed = dir.join("clip.subbed.mp4");
    let second =
        tafas::mp4write::write(&tracks, dimensions, &subbed).expect("the second write lands");
    assert!(first.bytes > 0 && second.bytes > 0);
    assert!(
        second.bytes > first.bytes.saturating_sub(64),
        "the second file carries the first one's samples plus a track"
    );

    let data = std::fs::read(&subbed).expect("the second file is on disk");
    let movie = mp4::read(&data).expect("the independent reader accepts the file");

    let subs = movie.track("subt").expect("there is a subtitle track");
    assert_eq!(subs.codec, "wvtt", "a WebVTT sample entry");
    assert_eq!(subs.timescale, 1_000, "cue times are milliseconds");
    assert_eq!(subs.samples.len(), 2, "one sample per cue");
    for sample in &subs.samples {
        assert!(
            movie.inside_mdat(sample),
            "a caption sample points into mdat, not at the header"
        );
    }

    assert_eq!(
        movie.track("vide").map(|track| track.samples.len()),
        Some(2),
        "the picture survived the rewrite"
    );
    assert_eq!(
        movie.track("soun").map(|track| track.samples.len()),
        Some(1),
        "the sound survived the rewrite"
    );

    // The cue text itself, byte for byte, inside the sample.
    let payl = data
        .windows(4)
        .position(|window| window == b"payl")
        .expect("a payl box carries the text");
    assert_eq!(&data[payl + 4..payl + 9], b"hello");
}
