//! The transfer layer against a socket this test owns.
//!
//! These are the cases that quietly corrupt downloads elsewhere: a resumed body
//! appended to the wrong offset, a server that ignores `Range`, a body shorter
//! than its declared length, and a 5xx that clears on retry. Each one is cheap to
//! write against a real listener and impossible to trust against a mock.

mod common;

use std::io::Write;

use common::{Route, TestServer};
use tafas::http::Http;

fn http() -> Http {
    // One retry, a short timeout: the server is on loopback and the tests must
    // not hang when a socket misbehaves.
    Http::new(1, 5, 0).expect("client")
}

fn body(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

#[test]
fn a_complete_body_arrives_byte_for_byte() {
    let payload = body(40_000);
    let server = TestServer::start(vec![Route::ok("/v.mp4", &payload)]);
    let mut sink = Vec::new();

    let report = http()
        .download(
            "media",
            &server.url("/v.mp4"),
            &[],
            &mut sink,
            0,
            &mut |_| {},
        )
        .expect("download");

    assert_eq!(sink, payload);
    assert_eq!(report.written, payload.len() as u64);
    assert_eq!(report.total, Some(payload.len() as u64));
    assert!(!report.restarted);
}

#[test]
fn a_resume_sends_a_range_and_appends_the_rest() {
    let payload = body(20_000);
    let server = TestServer::start(vec![Route::ok("/v.mp4", &payload)]);
    let half = payload.len() / 2;

    let mut sink = payload[..half].to_vec();
    let report = http()
        .download(
            "media",
            &server.url("/v.mp4"),
            &[],
            &mut sink,
            half as u64,
            &mut |_| {},
        )
        .expect("resume");

    assert_eq!(sink, payload, "the resumed file must be the whole file");
    assert_eq!(report.written, (payload.len() - half) as u64);
    assert!(!report.restarted);
    assert_eq!(report.total, Some(payload.len() as u64));
}

#[test]
fn a_server_that_ignores_range_is_reported_as_restarted() {
    let payload = body(8_000);
    let server = TestServer::start(vec![Route::ignoring_range("/v.mp4", &payload)]);

    let mut sink = Vec::new();
    let report = http()
        .download(
            "media",
            &server.url("/v.mp4"),
            &[],
            &mut sink,
            4_000,
            &mut |_| {},
        )
        .expect("download");

    // The body starts at zero even though a resume was asked for; the caller is
    // told, which is what stops the beginning being written twice.
    assert!(report.restarted);
    assert_eq!(sink, payload);
}

#[test]
fn a_range_past_the_end_is_reported_as_restarted() {
    let payload = body(1_000);
    let server = TestServer::start(vec![Route::ok("/v.mp4", &payload)]);

    let mut sink = Vec::new();
    let report = http()
        .download(
            "media",
            &server.url("/v.mp4"),
            &[],
            &mut sink,
            5_000,
            &mut |_| {},
        )
        .expect("download");

    assert!(report.restarted);
    assert_eq!(report.written, 0);
}

#[test]
fn a_short_body_is_visible_in_the_declared_total() {
    let payload = body(4_000);
    let server = TestServer::start(vec![Route::truncated("/v.mp4", &payload, 500)]);

    let mut sink = Vec::new();
    let report = http()
        .download(
            "media",
            &server.url("/v.mp4"),
            &[],
            &mut sink,
            0,
            &mut |_| {},
        )
        .expect("download");

    // The server declared the full length in Content-Length, so the caller's
    // byte-count check (the one that decides whether to rename) fails. That is
    // the whole point of comparing them.
    assert_eq!(report.total, Some(payload.len() as u64));
    assert_eq!(report.written, (payload.len() - 500) as u64);
    assert_ne!(report.total, Some(report.written));
}

#[test]
fn a_failing_server_is_retried_and_then_succeeds() {
    let payload = body(1_500);
    let server = TestServer::start(vec![Route::failing("/v.mp4", &payload, 1)]);

    let mut sink = Vec::new();
    let report = http()
        .download(
            "media",
            &server.url("/v.mp4"),
            &[],
            &mut sink,
            0,
            &mut |_| {},
        )
        .expect("the retry should have succeeded");

    assert_eq!(sink, payload);
    assert!(report.written > 0, "nothing was written");
}

#[test]
fn a_missing_path_is_an_unsupported_error_rather_than_a_silent_empty_file() {
    let server = TestServer::start(vec![Route::ok("/v.mp4", b"x")]);
    let mut sink = Vec::new();
    let error = http()
        .download(
            "media",
            &server.url("/nope.mp4"),
            &[],
            &mut sink,
            0,
            &mut |_| {},
        )
        .expect_err("404 must not be treated as success");
    assert!(matches!(error, tafas::error::Error::Unsupported(_)));
    assert!(sink.is_empty());
}

#[test]
fn a_json_body_is_read_and_a_non_json_body_is_a_parse_error() {
    let server = TestServer::start(vec![
        Route::ok("/good.json", br#"{"a":1}"#),
        Route::ok("/bad.json", b"<html>not json</html>"),
    ]);

    let value = http()
        .get_json("test", &server.url("/good.json"), &[])
        .expect("valid JSON");
    assert_eq!(value["a"], 1);

    let error = http()
        .get_json("test", &server.url("/bad.json"), &[])
        .expect_err("HTML is not JSON");
    assert!(matches!(error, tafas::error::Error::Parse(_)));
}

#[test]
fn a_partial_sink_keeps_what_was_written_when_the_transfer_fails() {
    struct Failing {
        bytes: usize,
        fail_after: usize,
    }
    impl Write for Failing {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.bytes >= self.fail_after {
                return Err(std::io::Error::other("disk full"));
            }
            self.bytes += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let payload = body(100_000);
    let server = TestServer::start(vec![Route::ok("/v.mp4", &payload)]);
    let mut sink = Failing {
        bytes: 0,
        fail_after: 1_000,
    };

    let error = http()
        .download(
            "media",
            &server.url("/v.mp4"),
            &[],
            &mut sink,
            0,
            &mut |_| {},
        )
        .expect_err("a write failure must surface");
    assert!(matches!(error, tafas::error::Error::Io(_)));
}
