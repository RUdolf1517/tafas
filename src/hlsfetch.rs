//! Turning a playlist into a file.
//!
//! `hls` reads the text and says what the segments are; `ts` and `mp4write`
//! know what to do with the bytes. This module is the middle: it fetches the
//! segments, concatenates them, demuxes the transport stream they carry and
//! hands the samples to the writer.
//!
//! Two things are done on purpose. The segments are held in memory one at a
//! time and the concatenation is thrown away as soon as the samples have been
//! lifted out of it, because a transport stream is a framing that an MP4 does
//! not need — keeping it would double the memory for nothing. And every gate
//! between a playlist and a fetch refuses **by name**: an fMP4 playlist, an
//! encrypted one and an unreadable one produce different messages, so the
//! reader learns what is missing instead of getting a half-written file.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

use crate::error::{Error, Result};
use crate::hls::{Encryption, Key, Media, Playlist, Segment};
use crate::http::Http;
use crate::items::Items;
use crate::mp4write;
use crate::select::Quality;
use crate::ts;
use crate::url::Url;

/// How much of a single segment this build will hold in memory. A segment is
/// seconds of video; anything past this is a mis-served playlist or a file
/// wearing a `.ts` extension, and naming the limit beats filling the machine.
pub const SEGMENT_LIMIT: u64 = 64 * 1024 * 1024;

/// The size of an AES-128 key, and the most a key file may be: a key URL that
/// answers with a megabyte is not a key.
const KEY_BYTES: usize = 16;
const KEY_LIMIT: u64 = 1024;

/// The most segments this build will have in flight at once. A segment may be
/// as large as `SEGMENT_LIMIT`, so this is a memory bound as much as a
/// politeness one: the bytes of every worker's segment are held until the
/// writer has them in order.
pub const CONCURRENCY_LIMIT: usize = 16;

/// How many segments a playlist may name before this build stops reading it.
/// Ten hours of six-second segments is six thousand, so this is slack, not a
/// policy.
pub const SEGMENT_COUNT_LIMIT: usize = 20_000;

/// What a finished HLS download contains.
#[derive(Debug, Clone)]
pub struct Report {
    /// Bytes of the file that was written, not bytes on the wire: the segments
    /// include transport-stream framing that the MP4 does not.
    pub bytes: u64,
    pub segments: usize,
    pub duration_ms: u64,
    /// The chosen variant's size, when the playlist was a master playlist.
    /// Zero when the playlist was already a segment list and said nothing.
    pub dimensions: (u32, u32),
    pub video_samples: u64,
    pub audio_samples: u64,
    /// Things the caller should say out loud but that are not failures — an
    /// item selection that starts between keyframes, for instance.
    pub warnings: Vec<String>,
}

/// Fetches `url` and writes an MP4 at `out`.
///
/// `headers` are the site's request headers (a referrer, usually); every
/// request in the walk carries them. `items` is `--playlist-items`: positions
/// into the segment list, 1-based, or `None` for all of them. `concurrency` is
/// `--concurrent-fragments`: how many segments of *one* list may be in flight
/// at once, clamped to `1..=CONCURRENCY_LIMIT`. `on_bytes` is called with the
/// size of each segment as it lands — in whatever order they land, because it
/// feeds a byte counter, not a file — so the caller's progress bar moves while
/// the file does not exist yet.
pub fn fetch(
    http: &Http,
    url: &Url,
    headers: &[(String, String)],
    quality: Quality,
    items: Option<&Items>,
    concurrency: usize,
    out: &Path,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<Report> {
    let concurrency = concurrency.clamp(1, CONCURRENCY_LIMIT);
    let borrowed: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();

    let text = http.get_text(&url.to_string(), &borrowed)?;
    let playlist = Playlist::parse(&text, url)?;

    let (video_url, audio_url, dimensions) = match &playlist {
        Playlist::Media(_) => (url.clone(), None, (0, 0)),
        Playlist::Master(master) => {
            let variant = master.choose(quality).ok_or_else(|| {
                Error::Parse(format!("{url}: the master playlist names no variants"))
            })?;
            let audio = master
                .audio_for(variant)
                .and_then(|rendition| rendition.uri.clone());
            (variant.uri.clone(), audio, (variant.width, variant.height))
        }
    };

    let video_media = media_playlist(http, &video_url, &borrowed)?;
    let video_segments = selected(&video_media, items, &video_url)?;
    let mut segments = 0usize;
    let video_bytes = fetch_media(
        http,
        &video_segments,
        &borrowed,
        concurrency,
        on_bytes,
        &mut segments,
    )?;
    let demuxed = ts::demux(&video_bytes)?;
    let mut duration = video_media.total_duration();
    let video_has_audio = demuxed.audio().is_some();
    let mut warnings = Vec::new();
    if items.is_some() {
        if let Some(first) = demuxed.video().and_then(|track| track.samples.first()) {
            if !first.keyframe {
                warnings.push(
                    "the selection does not begin on a keyframe: the file will not play from \
                     its first frame in every player"
                        .to_string(),
                );
            }
        }
    }
    let mut tracks = demuxed.tracks;

    // A variant with an audio group usually carries no audio of its own; when
    // it does, that audio is in sync with the video by construction, so the
    // separate rendition is not fetched over it.
    if let Some(audio_url) = audio_url.filter(|_| !video_has_audio) {
        let audio_media = media_playlist(http, &audio_url, &borrowed)?;
        let audio_segments = selected(&audio_media, items, &audio_url)?;
        let audio_bytes = fetch_media(
            http,
            &audio_segments,
            &borrowed,
            concurrency,
            on_bytes,
            &mut segments,
        )?;
        let audio = ts::demux(&audio_bytes)?;
        match audio.audio() {
            Some(track) => tracks.push(track.clone()),
            None => {
                return Err(Error::Parse(format!(
                    "{audio_url}: the audio rendition carries no audio track"
                )))
            }
        }
        duration = duration.max(audio_media.total_duration());
    }

    let report = mp4write::write(&tracks, dimensions, out)?;
    Ok(Report {
        bytes: report.bytes,
        segments,
        duration_ms: (duration * 1000.0).round() as u64,
        dimensions,
        video_samples: report.video_samples,
        audio_samples: report.audio_samples,
        warnings,
    })
}

/// One segment list, with the gates crossed.
fn media_playlist(http: &Http, url: &Url, headers: &[(&str, &str)]) -> Result<Media> {
    let text = http.get_text(&url.to_string(), headers)?;
    match Playlist::parse(&text, url)? {
        Playlist::Media(media) => {
            check(&media, url)?;
            Ok(media)
        }
        Playlist::Master(_) => Err(Error::Parse(format!(
            "{url}: expected a list of segments, found a list of variants"
        ))),
    }
}

/// The gates between a playlist and a fetch.
fn check(media: &Media, url: &Url) -> Result<()> {
    if let Some(map) = media
        .segments
        .iter()
        .find_map(|segment| segment.map.as_ref())
    {
        return Err(Error::Refused(format!(
            "{url}: this playlist uses fMP4 segments (EXT-X-MAP {}); tafas reads the MPEG-TS \
             flavour of HLS, and fMP4 is on the roadmap",
            map.uri
        )));
    }
    // AES-128 is read: the key is fetched once per URI and every segment is
    // decrypted in memory. The other methods are refused here, before a single
    // segment request, because they cannot be written as a file that is right.
    let method = media
        .encryption()
        .filter(|method| !matches!(*method, Encryption::None | Encryption::Aes128));
    if let Some(method) = method {
        return Err(Error::Refused(format!(
            "{url}: the segments are encrypted with {}, which this build cannot undo",
            method_name(method)
        )));
    }
    if media.segments.is_empty() {
        return Err(Error::Parse(format!(
            "{url}: the playlist lists no segments"
        )));
    }
    if media.segments.len() > SEGMENT_COUNT_LIMIT {
        return Err(Error::Limit(format!(
            "{url}: {} segments is past this build's limit of {SEGMENT_COUNT_LIMIT}",
            media.segments.len()
        )));
    }
    Ok(())
}

/// A key, fetched once per URI: a playlist repeats the same `EXT-X-KEY` before
/// every segment, and the file it names is the same file.
fn resolve_key(
    http: &Http,
    key: &Key,
    headers: &[(&str, &str)],
    cache: &mut HashMap<String, [u8; KEY_BYTES]>,
) -> Result<[u8; KEY_BYTES]> {
    let uri = key
        .uri
        .clone()
        .ok_or_else(|| Error::Parse("an AES-128 key block without a URI".to_string()))?;
    let id = uri.to_string();
    if let Some(cached) = cache.get(&id) {
        return Ok(*cached);
    }
    let bytes = http.get_bytes("hls-key", &id, headers, None, KEY_LIMIT)?;
    let key_bytes: [u8; KEY_BYTES] = bytes.as_slice().try_into().map_err(|_| {
        Error::Refused(format!(
            "{id}: an AES-128 key is {KEY_BYTES} bytes, this one is {}",
            bytes.len()
        ))
    })?;
    cache.insert(id, key_bytes);
    Ok(key_bytes)
}

/// One segment, decrypted in place.
///
/// HLS specifies AES-128 in CBC mode with PKCS#7 padding for whole segments.
/// A segment that does not decrypt — a wrong key, a wrong IV, a truncated
/// body — is a failure rather than something to write out and hope for: the
/// padding check is the only integrity signal there is.
fn decrypt(data: &[u8], key: [u8; KEY_BYTES], iv: [u8; 16]) -> Result<Vec<u8>> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

    let mut buffer = data.to_vec();
    let clear_length = cbc::Decryptor::<aes::Aes128>::new(&key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|_| {
            Error::Verify(
                "an HLS segment did not decrypt: the key, the IV or the padding is wrong"
                    .to_string(),
            )
        })?
        .len();
    buffer.truncate(clear_length);
    Ok(buffer)
}

fn method_name(method: Encryption) -> &'static str {
    match method {
        Encryption::None => "METHOD=NONE",
        Encryption::Aes128 => "AES-128",
        Encryption::SampleAes => "SAMPLE-AES",
        Encryption::Other => "an unknown method",
    }
}

/// The segments an item selection keeps, in playlist order.
///
/// A selection that matches nothing is a refusal: writing an empty file and
/// calling it success is the one thing this program does not do.
fn selected<'a>(media: &'a Media, items: Option<&Items>, url: &Url) -> Result<Vec<&'a Segment>> {
    let chosen: Vec<&Segment> = media
        .segments
        .iter()
        .enumerate()
        .filter(|(index, _)| match items {
            Some(items) => items.contains(index + 1),
            None => true,
        })
        .map(|(_, segment)| segment)
        .collect();

    if let (true, Some(items)) = (chosen.is_empty(), items) {
        return Err(Error::Refused(format!(
            "--playlist-items {items} selects none of the {} segments of {url}",
            media.segments.len()
        )));
    }
    if chosen.is_empty() {
        return Err(Error::Refused(format!(
            "{url}: the playlist names no segments"
        )));
    }
    Ok(chosen)
}

/// One segment, fetched and — if the playlist says so — decrypted.
fn fetch_segment(
    http: &Http,
    segment: &Segment,
    headers: &[(&str, &str)],
    key: Option<([u8; KEY_BYTES], [u8; 16])>,
) -> Result<Vec<u8>> {
    let range = segment.range.map(|range| (range.offset, range.length));
    let mut bytes = http.get_bytes(
        "hls-segment",
        &segment.uri.to_string(),
        headers,
        range,
        SEGMENT_LIMIT,
    )?;
    if let Some((key_bytes, iv)) = key {
        bytes = decrypt(&bytes, key_bytes, iv)?;
    }
    Ok(bytes)
}

/// Every segment of one playlist, concatenated.
///
/// Transport streams are packet-aligned, so a segment boundary is always
/// between two 188-byte packets and the concatenation is itself a stream.
///
/// With more than one worker the segments are fetched out of order and written
/// into their own slot, so the concatenation below is in playlist order however
/// the arrivals interleave. One failure cancels the rest: the remaining
/// segments would be bytes nobody writes.
fn fetch_media(
    http: &Http,
    chosen: &[&Segment],
    headers: &[(&str, &str)],
    concurrency: usize,
    on_bytes: &mut dyn FnMut(u64),
    segments: &mut usize,
) -> Result<Vec<u8>> {
    // Keys are resolved before any worker starts. A playlist repeats the same
    // `EXT-X-KEY` before every segment, so the cache is filled once, in order,
    // and is read-only afterwards: no lock in the fetch path, and no key
    // fetched twice because two workers reached the same URI together.
    let mut cache: HashMap<String, [u8; KEY_BYTES]> = HashMap::new();
    let mut keys: Vec<Option<([u8; KEY_BYTES], [u8; 16])>> = Vec::with_capacity(chosen.len());
    for segment in chosen {
        let resolved = match segment
            .key
            .as_ref()
            .filter(|key| key.method == Encryption::Aes128)
        {
            // The IV belongs to the segment, not to the key block: a playlist
            // that omits `IV` expects the media sequence number, which differs
            // per segment. Both are settled here so the fetch path stays a
            // fetch.
            Some(key) => Some((
                resolve_key(http, key, headers, &mut cache)?,
                key.iv_for(segment.sequence),
            )),
            None => None,
        };
        keys.push(resolved);
    }

    let workers = concurrency.clamp(1, chosen.len().max(1));
    let mut pieces: Vec<Option<Vec<u8>>> = (0..chosen.len()).map(|_| None).collect();
    let mut failure: Option<Error> = None;

    if workers == 1 {
        for (index, (segment, key)) in chosen.iter().zip(keys.iter()).enumerate() {
            let bytes = fetch_segment(http, *segment, headers, *key)?;
            on_bytes(bytes.len() as u64);
            pieces[index] = Some(bytes);
        }
    } else {
        let next = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        let (sender, receiver) = mpsc::channel();
        thread::scope(|scope| {
            for _ in 0..workers {
                let sender = sender.clone();
                let next = &next;
                let cancelled = &cancelled;
                let keys = &keys;
                scope.spawn(move || loop {
                    if cancelled.load(Ordering::SeqCst) {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    if index >= chosen.len() {
                        break;
                    }
                    let outcome = fetch_segment(http, chosen[index], headers, keys[index]);
                    if outcome.is_err() {
                        cancelled.store(true, Ordering::SeqCst);
                    }
                    if sender.send((index, outcome)).is_err() {
                        break;
                    }
                });
            }
            // The original sender has to go or `iter` below never ends.
            drop(sender);
            for (index, outcome) in receiver.iter() {
                match outcome {
                    Ok(bytes) => {
                        on_bytes(bytes.len() as u64);
                        pieces[index] = Some(bytes);
                    }
                    Err(error) => {
                        if failure.is_none() {
                            failure = Some(error);
                        }
                    }
                }
            }
        });
    }

    if let Some(error) = failure {
        return Err(error);
    }

    let mut data = Vec::new();
    for (index, piece) in pieces.into_iter().enumerate() {
        let bytes = piece.ok_or_else(|| {
            Error::Internal(format!(
                "segment {} of {} was never fetched",
                index + 1,
                chosen.len()
            ))
        })?;
        data.extend_from_slice(&bytes);
    }
    *segments += chosen.len();
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_aes128_playlist_passes_the_gate() {
        let url = Url::parse("https://example.test/media.m3u8").unwrap();
        let text = "#EXTM3U\n\
                    #EXT-X-TARGETDURATION:6\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n\
                    #EXTINF:6.0,\n\
                    seg1.ts\n\
                    #EXT-X-ENDLIST\n";
        let media = match Playlist::parse(text, &url).unwrap() {
            Playlist::Media(media) => media,
            Playlist::Master(_) => panic!("expected a segment list"),
        };
        assert_eq!(media.encryption(), Some(Encryption::Aes128));
        assert!(check(&media, &url).is_ok(), "AES-128 is read, not refused");
    }

    #[test]
    fn a_segment_that_does_not_decrypt_is_a_failure() {
        // A body that is not a whole number of blocks cannot be a padded
        // CBC stream, whatever the key says. The padding check is the only
        // integrity signal an AES-128 segment carries.
        let error = decrypt(&[0x42; 17], [0u8; 16], [0u8; 16]).unwrap_err();
        assert!(error.to_string().contains("did not decrypt"), "{error}");
    }

    #[test]
    fn an_fmp4_playlist_is_refused_by_name() {
        let url = Url::parse("https://example.test/media.m3u8").unwrap();
        let text = "#EXTM3U\n\
                    #EXT-X-MAP:URI=\"init.mp4\"\n\
                    #EXTINF:6.0,\n\
                    seg1.m4s\n\
                    #EXT-X-ENDLIST\n";
        let media = match Playlist::parse(text, &url).unwrap() {
            Playlist::Media(media) => media,
            Playlist::Master(_) => panic!("expected a segment list"),
        };
        let error = check(&media, &url).unwrap_err();
        assert!(error.to_string().contains("fMP4"), "{error}");
    }

    #[test]
    fn a_playlist_without_segments_is_refused() {
        let url = Url::parse("https://example.test/media.m3u8").unwrap();
        let media = Media {
            segments: Vec::new(),
            target_duration: 6,
            end_list: true,
            playlist_type: crate::hls::PlaylistType::Vod,
        };
        let error = check(&media, &url).unwrap_err();
        assert!(error.to_string().contains("no segments"), "{error}");
    }
}
