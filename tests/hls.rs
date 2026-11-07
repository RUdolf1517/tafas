//! End-to-end HLS: a playlist on a socket, segments behind it, an MP4 on disk.
//!
//! `hls`, `ts` and `mp4write` have unit tests of their own; this file is about
//! the seam between them and the network, which is where a pipeline actually
//! breaks — a relative segment URI, a byte-range window, a playlist that
//! promises fMP4. The transport stream is built by hand in `tests/common`, so
//! nothing here needs a real video.

mod common;

use common::{fmp4_fragment, fmp4_init, fmp4_playlist, temp_dir, ts_segment, Route, TestServer};
use tafas::hlsfetch;
use tafas::hlsfetch::Segments;
use tafas::http::Http;
use tafas::items::Items;
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
    let report = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |bytes| seen += bytes,
    )
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
        Segments::default(),
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
fn an_fmp4_playlist_becomes_an_mp4() {
    let init = fmp4_init();
    let first = fmp4_fragment(0, &[(b"first-sample", true)]);
    let second = fmp4_fragment(3_000, &[(b"second-sample", false)]);
    let playlist = fmp4_playlist(&["seg1.m4s", "seg2.m4s"]);
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", playlist.as_bytes()),
        Route::ok("/init.mp4", &init),
        Route::ok("/seg1.m4s", &first),
        Route::ok("/seg2.m4s", &second),
    ]);
    let out = temp_dir("hls-fmp4").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let mut seen = 0u64;
    let report = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |bytes| seen += bytes,
    )
    .expect("a fragmented-MP4 playlist should be read");

    assert_eq!(report.segments, 2);
    assert_eq!(report.video_samples, 2);
    assert_eq!(report.audio_samples, 0);
    assert_eq!(
        seen,
        (init.len() + first.len() + second.len()) as u64,
        "the header is bytes on the wire too, so the counter counts it"
    );
    assert_eq!(
        server.hits("/init.mp4"),
        1,
        "the header is fetched once, not once per segment"
    );
    mux::verify(&out).expect("the written file verifies");
}

#[test]
fn an_fmp4_playlist_is_parsed_against_the_header_it_names() {
    // Two segments, one of them empty of samples: the sample count comes from
    // the fragments, not from the number of segments.
    let init = fmp4_init();
    let busy = fmp4_fragment(0, &[(b"one", true), (b"two", false), (b"three", false)]);
    let playlist = fmp4_playlist(&["seg1.m4s"]);
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", playlist.as_bytes()),
        Route::ok("/init.mp4", &init),
        Route::ok("/seg1.m4s", &busy),
    ]);
    let out = temp_dir("hls-fmp4-samples").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let report = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |_| {},
    )
    .expect("three samples in one fragment are still one segment");
    assert_eq!(report.segments, 1);
    assert_eq!(report.video_samples, 3);
    mux::verify(&out).expect("the written file verifies");
}

#[test]
fn a_header_that_is_not_mp4_is_refused_by_name() {
    let playlist = fmp4_playlist(&["seg1.m4s"]);
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", playlist.as_bytes()),
        Route::ok("/init.mp4", b"not an mp4 at all, just text"),
        Route::ok("/seg1.m4s", &fmp4_fragment(0, &[(b"x", true)])),
    ]);
    let out = temp_dir("hls-fmp4-bad-init").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let error = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |_| {},
    )
    .expect_err("a header that is not a header is a failure");
    let text = error.to_string();
    assert!(text.contains("moov") || text.contains("box"), "{text}");
    assert!(!out.exists(), "a refusal leaves no half-written file");
}

#[test]
fn a_playlist_that_changes_its_header_is_refused() {
    let text = "#EXTM3U\n\
                #EXT-X-TARGETDURATION:6\n\
                #EXT-X-MAP:URI=\"init.mp4\"\n\
                #EXTINF:6.0,\n\
                seg1.m4s\n\
                #EXT-X-MAP:URI=\"other-init.mp4\"\n\
                #EXTINF:6.0,\n\
                seg2.m4s\n\
                #EXT-X-ENDLIST\n";
    let server = TestServer::start(vec![Route::ok("/media.m3u8", text.as_bytes())]);
    let out = temp_dir("hls-fmp4-two-maps").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let error = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |_| {},
    )
    .expect_err("two headers describe two track sets");
    let message = error.to_string();
    assert!(message.contains("EXT-X-MAP"), "{message}");
    assert!(message.contains("segment 2"), "{message}");
    assert!(!out.exists());
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

    let report = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |_| {},
    )
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

    let error = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |_| {},
    )
    .expect_err("a 200 in place of a 206 means the window is wrong");
    assert!(
        error.to_string().contains("byte-range"),
        "the refusal should name the byte range, not the codec: {error}"
    );
    assert!(!out.exists());
}

#[test]
fn aes128_segments_are_decrypted_and_the_key_is_fetched_once() {
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

    let report = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |_| {},
    )
    .expect("an AES-128 playlist should decrypt into the same file");

    assert_eq!(report.segments, 2);
    assert_eq!(report.video_samples, 2);
    assert_eq!(report.audio_samples, 2);
    let verified = mux::verify(&out).expect("the assembled file should be readable");
    assert_eq!(verified.sample_total(), 4);

    // The key URI repeats before every segment and names the same file every
    // time, so it is fetched once.
    assert_eq!(server.hits("/key.bin"), 1);
    assert_eq!(server.hits("/seg1.ts"), 1);
    assert_eq!(server.hits("/seg2.ts"), 1);
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

    let error = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |_| {},
    )
    .expect_err("eight bytes are not a key");
    let text = error.to_string();
    assert!(text.contains("16 bytes"), "{text}");
    assert!(!out.exists());
}

#[test]
fn a_playlist_selection_fetches_only_the_segments_it_names() {
    let segment = ts_segment();
    let playlist = "#EXTM3U\n\
                    #EXT-X-TARGETDURATION:6\n\
                    #EXTINF:2.0,\n\
                    seg1.ts\n\
                    #EXTINF:2.0,\n\
                    seg2.ts\n\
                    #EXTINF:2.0,\n\
                    seg3.ts\n\
                    #EXT-X-ENDLIST\n";
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", playlist.as_bytes()),
        Route::ok("/seg1.ts", &segment),
        Route::ok("/seg2.ts", &segment),
        Route::ok("/seg3.ts", &segment),
    ]);
    let out = temp_dir("hls-items").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();
    let items = Items::parse("1").unwrap();

    let report = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments {
            items: Some(&items),
            ..Segments::default()
        },
        &out,
        &mut |_| {},
    )
    .expect("one segment out of three is still a stream");

    assert_eq!(report.segments, 1);
    assert_eq!(server.hits("/seg1.ts"), 1);
    assert_eq!(
        server.hits("/seg2.ts"),
        0,
        "an unselected segment is not fetched"
    );
    assert_eq!(server.hits("/seg3.ts"), 0);
    let verified = mux::verify(&out).expect("the assembled file should be readable");
    assert_eq!(verified.sample_total(), 4);
}

#[test]
fn a_selection_that_matches_nothing_is_refused() {
    let segment = ts_segment();
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", media_playlist("seg1.ts").as_bytes()),
        Route::ok("/seg1.ts", &segment),
    ]);
    let out = temp_dir("hls-items-empty").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();
    let items = Items::parse("4-9").unwrap();

    let error = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments {
            items: Some(&items),
            ..Segments::default()
        },
        &out,
        &mut |_| {},
    )
    .expect_err("a playlist with one segment cannot satisfy 4-9");
    let text = error.to_string();
    assert!(text.contains("4-9"), "{text}");
    assert!(text.contains("selects none"), "{text}");
    assert!(!out.exists());
}

/// The file the pipeline wrote, read by a parser that shares no code with the
/// writer: the guarantee `docs/guarantees.md` §6 makes, exercised on the one
/// path that builds a file from nothing.
#[test]
fn an_independent_reader_agrees_with_the_written_file() {
    let segment = ts_segment();
    let server = TestServer::start(vec![
        Route::ok("/media.m3u8", media_playlist("seg1.ts").as_bytes()),
        Route::ok("/seg1.ts", &segment),
    ]);
    let out = temp_dir("hls-independent").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |_| {},
    )
    .expect("the pipeline should accept a plain media playlist");

    let bytes = std::fs::read(&out).expect("the file is on disk");
    let movie = common::mp4::read(&bytes).expect("the independent reader accepts it");

    assert_eq!(movie.tracks.len(), 2, "one video track and one audio track");
    let video = movie.track("vide").expect("a video track");
    let audio = movie.track("soun").expect("an audio track");
    assert_eq!(video.codec, "avc1", "the sample description says H.264");
    assert_eq!(audio.codec, "mp4a", "and the audio one says AAC");
    assert_eq!(video.samples.len(), 2);
    assert_eq!(audio.samples.len(), 2);

    for sample in video.samples.iter().chain(audio.samples.iter()) {
        assert!(
            movie.inside_mdat(sample),
            "a sample at {} with {} bytes is outside every mdat",
            sample.offset,
            sample.size
        );
        assert!(
            sample.size > 0,
            "an empty sample is a rewrite that lost data"
        );
    }
    assert_eq!(video.keyframes(), 2, "both slices are IDRs");
}

/// A playlist of `count` segments whose durations differ, so the total the
/// writer reports proves the list was read in order.
fn playlist_of(count: usize) -> String {
    let mut text = String::from(
        "#EXTM3U\n\
         #EXT-X-VERSION:3\n\
         #EXT-X-TARGETDURATION:24\n\
         #EXT-X-MEDIA-SEQUENCE:0\n",
    );
    for index in 1..=count {
        text.push_str(&format!("#EXTINF:{}.0,\nseg{index}.ts\n", index * 6));
    }
    text.push_str("#EXT-X-ENDLIST\n");
    text
}

/// Four segments, each of which the server holds for 80 ms. Four workers are
/// asked for, so the server must see more than one request at a time; the file
/// must still be the four segments in playlist order.
#[test]
fn segments_are_fetched_in_parallel_when_more_than_one_worker_is_asked_for() {
    let segment = ts_segment();
    let count = 4usize;
    let mut routes = vec![Route::ok("/media.m3u8", playlist_of(count).as_bytes())];
    for index in 1..=count {
        routes.push(Route::slow(&format!("/seg{index}.ts"), &segment, 80));
    }
    let server = TestServer::start(routes);
    let out = temp_dir("hls-parallel").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let mut seen = 0u64;
    let report = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments {
            concurrent: 4,
            ..Segments::default()
        },
        &out,
        &mut |bytes| seen += bytes,
    )
    .expect("four segments and four workers");

    assert_eq!(report.segments, 4);
    assert_eq!(
        seen,
        (segment.len() * count) as u64,
        "every byte counted once"
    );
    assert!(
        server.peak_concurrent() >= 2,
        "the segments overlapped on the server: peak {}",
        server.peak_concurrent()
    );
    assert_eq!(
        report.duration_ms, 60_000,
        "6 + 12 + 18 + 24 seconds, in the order the playlist names them"
    );
    let verified = mux::verify(&out).expect("the assembled file should be readable");
    assert_eq!(
        verified.sample_total(),
        4 * count as u64,
        "every segment contributed its samples"
    );
    for index in 1..=count {
        assert_eq!(
            server.hits(&format!("/seg{index}.ts")),
            1,
            "segment {index}"
        );
    }
}

/// The default: one request at a time, on the same fixtures, so the flag is
/// what changes the behaviour and not the fixture.
#[test]
fn one_worker_fetches_one_segment_at_a_time() {
    let segment = ts_segment();
    let count = 4usize;
    let mut routes = vec![Route::ok("/media.m3u8", playlist_of(count).as_bytes())];
    for index in 1..=count {
        routes.push(Route::slow(&format!("/seg{index}.ts"), &segment, 30));
    }
    let server = TestServer::start(routes);
    let out = temp_dir("hls-serial").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let report = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments::default(),
        &out,
        &mut |_| {},
    )
    .expect("four segments, one at a time");

    assert_eq!(report.segments, 4);
    assert_eq!(
        server.peak_concurrent(),
        1,
        "with one worker the server never sees two requests at once"
    );
    let verified = mux::verify(&out).expect("the assembled file should be readable");
    assert_eq!(verified.sample_total(), 4 * count as u64);
}

/// A failing segment stops the walk rather than fetching the rest of a
/// playlist whose output would be thrown away.
#[test]
fn a_failed_segment_cancels_the_segments_that_are_still_queued() {
    let segment = ts_segment();
    let count = 4usize;
    // `seg1.ts` has no route, so the server answers 404: a failure on the very
    // first segment, with three more already queued behind it.
    // The slow segments have to outlast the failing one by a wide margin, or the
    // assertion below becomes a race: a worker that finishes a 40 ms segment can
    // reach for the next one before the 404 on `seg1.ts` has been processed, and
    // that is a scheduling fact about the machine rather than a bug. 400 ms is
    // an order of magnitude past anything a 404 needs on a loaded runner.
    let routes = vec![
        Route::ok("/media.m3u8", playlist_of(count).as_bytes()),
        Route::slow("/seg2.ts", &segment, 400),
        Route::slow("/seg3.ts", &segment, 400),
        Route::slow("/seg4.ts", &segment, 400),
    ];
    let server = TestServer::start(routes);
    let out = temp_dir("hls-cancel").join("clip.mp4");
    let url = Url::parse(&server.url("/media.m3u8")).unwrap();

    let error = hlsfetch::fetch(
        &client(),
        &url,
        &[],
        Quality::Best,
        Segments {
            concurrent: 2,
            ..Segments::default()
        },
        &out,
        &mut |_| {},
    )
    .expect_err("a 404 on a segment is a failure");
    let text = error.to_string();
    assert!(text.contains("seg1.ts"), "{text}");
    assert!(!out.exists(), "nothing is left on disk");
    assert_eq!(
        server.hits("/seg4.ts"),
        0,
        "the queue stopped when the first worker failed"
    );
    assert!(
        server.hits("/seg3.ts") <= 1,
        "at most the one segment already in flight may have been started"
    );
}
