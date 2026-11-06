//! End-to-end HLS: a playlist on a socket, segments behind it, an MP4 on disk.
//!
//! `hls`, `ts` and `mp4write` have unit tests of their own; this file is about
//! the seam between them and the network, which is where a pipeline actually
//! breaks — a relative segment URI, a byte-range window, a playlist that
//! promises fMP4. The transport stream is built by hand in `tests/common`, so
//! nothing here needs a real video.

mod common;

use common::{temp_dir, ts_segment, Route, TestServer};
use tafas::hlsfetch;
use tafas::http::Http;
use tafas::mux;
use tafas::select::Quality;
use tafas::url::Url;

fn client() -> Http {
    Http::new(1, 5, 0).expect("client")
}

/// The playlist a single-file HLS post looks like.
fn media_playlist(segment: &str) -> String {
    format!(
        "#EXTM3U\n\
         #EXT-X-VERSION:3\n\
         #EXT-X-TARGETDURATION:6\n\
         #EXT-X-MEDIA-SEQUENCE:0\n\
         #EXTINF:6.0,\n\
         {segment}\n\
         #EXT-X-ENDLIST\n"
    )
}

#[test]
fn a_media_playlist_becomes_an_mp4() {
    let segment = ts_segment();
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", media_playlist("seg1.ts").as_bytes()),
        Route::ok("/seg1.ts", &segment),
    ]);
    let out = temp_dir("hls-media").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let mut seen = 0u64;
    let report = hlsfetch::fetch(&client(), &url, &[], Quality::Best, &out, &mut |bytes| {
        seen += bytes
    })
    .expect("the pipeline should accept a plain media playlist");

    assert_eq!(report.segments, 1);
    assert_eq!(
        seen,
        segment.len() as u64,
        "progress counts the bytes on the wire"
    );
    assert_eq!(report.video_samples, 2);
    assert_eq!(report.audio_samples, 2);
    assert_eq!(report.duration_ms, 6_000);

    let verified = mux::verify(&out).expect("the written file should be readable");
    assert!(verified.has_handler("vide"), "a video track");
    assert!(verified.has_handler("soun"), "an audio track");
    assert_eq!(verified.sample_total(), 4);
}

#[test]
fn a_master_playlist_is_walked_to_the_chosen_variant() {
    let segment = ts_segment();
    let master = "#EXTM3U\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n\
                  low.m3u8\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=4000000,RESOLUTION=1920x1080\n\
                  high.m3u8\n";
    let server = TestServer::start(vec![
        Route::ok("/master.m3u8", master.as_bytes()),
        Route::ok("/low.m3u8", media_playlist("low.ts").as_bytes()),
        Route::ok("/high.m3u8", media_playlist("high.ts").as_bytes()),
        Route::ok("/low.ts", &segment),
        Route::ok("/high.ts", &segment),
    ]);
    let out = temp_dir("hls-master").join("clip.mp4");
    let url = Url::parse(&server.url("/master.m3u8")).unwrap();

    let report = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Height(720),
        &out,
        &mut |_| {},
    )
    .expect("a 720p ceiling should find the 360p variant");

    assert_eq!(
        report.dimensions,
        (640, 360),
        "the height ceiling rules out the 1080p variant"
    );
    assert!(out.exists());
}

#[test]
fn an_fmp4_playlist_is_refused_before_a_file_appears() {
    let playlist = "#EXTM3U\n\
                    #EXT-X-MAP:URI=\"init.mp4\"\n\
                    #EXT-X-TARGETDURATION:6\n\
                    #EXTINF:6.0,\n\
                    seg1.m4s\n\
                    #EXT-X-ENDLIST\n";
    let server = TestServer::start(vec![Route::ok("/media.m3u8", playlist.as_bytes())]);
    let out = temp_dir("hls-fmp4").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let error = hlsfetch::fetch(&client(), &url, &[], Quality::Best, &out, &mut |_| {})
        .expect_err("fMP4 segments are not read by this build");
    let text = error.to_string();
    assert!(text.contains("fMP4"), "{text}");
    assert!(text.contains("EXT-X-MAP"), "{text}");
    assert!(!out.exists(), "a refusal leaves no half-written file");
}

#[test]
fn byte_ranged_segments_are_cut_out_of_one_object() {
    let segment = ts_segment();
    // Six packets: the tables are two of them, the media the other four.
    assert_eq!(segment.len(), 6 * 188);
    let playlist = "#EXTM3U\n\
                    #EXT-X-TARGETDURATION:6\n\
                    #EXT-X-BYTERANGE:376@0\n\
                    #EXTINF:6.0,\n\
                    seg.ts\n\
                    #EXT-X-BYTERANGE:752@376\n\
                    #EXTINF:6.0,\n\
                    seg.ts\n\
                    #EXT-X-ENDLIST\n";
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", playlist.as_bytes()),
        Route::ok("/seg.ts", &segment),
    ]);
    let out = temp_dir("hls-range").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let report = hlsfetch::fetch(&client(), &url, &[], Quality::Best, &out, &mut |_| {})
        .expect("a byte-range playlist should be assembled");

    assert_eq!(report.segments, 2);
    assert_eq!(report.video_samples, 2);
    let verified = mux::verify(&out).expect("the assembled file should be readable");
    assert_eq!(verified.sample_total(), 4);
}

#[test]
fn a_server_that_ignores_byte_ranges_is_refused() {
    let segment = ts_segment();
    let playlist = "#EXTM3U\n\
                    #EXT-X-TARGETDURATION:6\n\
                    #EXT-X-BYTERANGE:752@376\n\
                    #EXTINF:6.0,\n\
                    seg.ts\n\
                    #EXT-X-ENDLIST\n";
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", playlist.as_bytes()),
        Route::ignoring_range("/seg.ts", &segment),
    ]);
    let out = temp_dir("hls-range-ignored").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let error = hlsfetch::fetch(&client(), &url, &[], Quality::Best, &out, &mut |_| {})
        .expect_err("a 200 in place of a 206 means the window is wrong");
    assert!(
        error.to_string().contains("byte-range"),
        "the refusal should name the byte range, not the codec: {error}"
    );
    assert!(!out.exists());
}
