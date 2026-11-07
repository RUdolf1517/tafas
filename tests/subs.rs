//! Captions over a socket: the writer, the refusals, and the file names.
//!
//! The parser and the selection rules are unit-tested in `src/subtitles.rs`.
//! What is tested here is the part that talks to a server and to the disk: a
//! body that is written where it should be, and the two bodies that must not
//! become files at all.

mod common;

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
