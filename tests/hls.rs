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

/// AES-128-CBC with PKCS#7 padding, the way an HLS segment is encrypted.
fn encrypt(clear: &[u8], key: [u8; 16], iv: [u8; 16]) -> Vec<u8> {
    use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

    let mut buffer = vec![0u8; clear.len() + 16];
    buffer[..clear.len()].copy_from_slice(clear);
    let written = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, clear.len())
        .expect("a buffer with one spare block can hold the padding")
        .len();
    buffer.truncate(written);
    buffer
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

#[test]
fn aes128_segments_are_decrypted_and_the_key_is_fetched_once() {
    use std::sync::atomic::Ordering;

    const KEY: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const IV: [u8; 16] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];

    // Two segments, split at a packet boundary, each encrypted on its own with
    // the same key and the same explicit IV.
    let stream = ts_segment();
    let first = encrypt(&stream[..3 * 188], KEY, IV);
    let second = encrypt(&stream[3 * 188..], KEY, IV);

    let playlist = "#EXTM3U\n\
                    #EXT-X-TARGETDURATION:6\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",\
                    IV=0x101112131415161718191a1b1c1d1e1f\n\
                    #EXTINF:3.0,\n\
                    seg1.ts\n\
                    #EXTINF:3.0,\n\
                    seg2.ts\n\
                    #EXT-X-ENDLIST\n";
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", playlist.as_bytes()),
        Route::ok("/key.bin", &KEY),
        Route::ok("/seg1.ts", &first),
        Route::ok("/seg2.ts", &second),
    ]);
    let out = temp_dir("hls-aes").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let report = hlsfetch::fetch(&client(), &url, &[], Quality::Best, &out, &mut |_| {})
        .expect("an AES-128 playlist should decrypt into the same file");

    assert_eq!(report.segments, 2);
    assert_eq!(report.video_samples, 2);
    assert_eq!(report.audio_samples, 2);
    let verified = mux::verify(&out).expect("the assembled file should be readable");
    assert_eq!(verified.sample_total(), 4);

    // One playlist, two segments, one key: the key URI repeats before every
    // segment and is the same file every time.
    assert!(
        server.requests.load(Ordering::SeqCst) <= 4,
        "the key was fetched more than once"
    );
}

#[test]
fn a_key_of_the_wrong_size_is_refused() {
    let segment = ts_segment();
    let playlist = "#EXTM3U\n\
                    #EXT-X-TARGETDURATION:6\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n\
                    #EXTINF:6.0,\n\
                    seg1.ts\n\
                    #EXT-X-ENDLIST\n";
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", playlist.as_bytes()),
        Route::ok("/key.bin", &[0x11; 8]),
        Route::ok("/seg1.ts", &segment),
    ]);
    let out = temp_dir("hls-aes-short-key").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let error = hlsfetch::fetch(&client(), &url, &[], Quality::Best, &out, &mut |_| {})
        .expect_err("eight bytes are not a key");
    let text = error.to_string();
    assert!(text.contains("16 bytes"), "{text}");
    assert!(!out.exists());
}
