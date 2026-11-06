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

use std::path::Path;

use crate::error::{Error, Result};
use crate::hls::{Encryption, Media, Playlist};
use crate::http::Http;
use crate::mp4write;
use crate::select::Quality;
use crate::ts;
use crate::url::Url;

/// How much of a single segment this build will hold in memory. A segment is
/// seconds of video; anything past this is a mis-served playlist or a file
/// wearing a `.ts` extension, and naming the limit beats filling the machine.
pub const SEGMENT_LIMIT: u64 = 64 * 1024 * 1024;

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
}

/// Fetches `url` and writes an MP4 at `out`.
///
/// `headers` are the site's request headers (a referrer, usually); every
/// request in the walk carries them. `on_bytes` is called with the size of each
/// segment as it lands, so the caller's progress bar moves while the file does
/// not exist yet.
pub fn fetch(
    http: &Http,
    url: &Url,
    headers: &[(String, String)],
    quality: Quality,
    out: &Path,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<Report> {
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
    let mut segments = 0usize;
    let video_bytes = fetch_media(http, &video_media, &borrowed, on_bytes, &mut segments)?;
    let demuxed = ts::demux(&video_bytes)?;
    let mut duration = video_media.total_duration();
    let video_has_audio = demuxed.audio().is_some();
    let mut tracks = demuxed.tracks;

    // A variant with an audio group usually carries no audio of its own; when
    // it does, that audio is in sync with the video by construction, so the
    // separate rendition is not fetched over it.
    if let Some(audio_url) = audio_url.filter(|_| !video_has_audio) {
        let audio_media = media_playlist(http, &audio_url, &borrowed)?;
        let audio_bytes = fetch_media(http, &audio_media, &borrowed, on_bytes, &mut segments)?;
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
    if let Some(map) = media.segments.iter().find_map(|segment| segment.map.as_ref()) {
        return Err(Error::Refused(format!(
            "{url}: this playlist uses fMP4 segments (EXT-X-MAP {}); tafas reads the MPEG-TS \
             flavour of HLS, and fMP4 is on the roadmap",
            map.uri
        )));
    }
    let method = media
        .encryption()
        .filter(|method| *method != Encryption::None);
    if let Some(method) = method {
        return Err(Error::Refused(format!(
            "{url}: the segments are encrypted with {}; decryption is not in this build yet",
            method_name(method)
        )));
    }
    if media.segments.is_empty() {
        return Err(Error::Parse(format!("{url}: the playlist lists no segments")));
    }
    if media.segments.len() > SEGMENT_COUNT_LIMIT {
        return Err(Error::Limit(format!(
            "{url}: {} segments is past this build's limit of {SEGMENT_COUNT_LIMIT}",
            media.segments.len()
        )));
    }
    Ok(())
}

fn method_name(method: Encryption) -> &'static str {
    match method {
        Encryption::None => "METHOD=NONE",
        Encryption::Aes128 => "AES-128",
        Encryption::SampleAes => "SAMPLE-AES",
        Encryption::Other => "an unknown method",
    }
}

/// Every segment of one playlist, concatenated.
///
/// Transport streams are packet-aligned, so a segment boundary is always
/// between two 188-byte packets and the concatenation is itself a stream.
fn fetch_media(
    http: &Http,
    media: &Media,
    headers: &[(&str, &str)],
    on_bytes: &mut dyn FnMut(u64),
    segments: &mut usize,
) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    for segment in &media.segments {
        let range = segment.range.map(|range| (range.offset, range.length));
        let bytes = http.get_bytes(
            "hls-segment",
            &segment.uri.to_string(),
            headers,
            range,
            SEGMENT_LIMIT,
        )?;
        on_bytes(bytes.len() as u64);
        data.extend_from_slice(&bytes);
        *segments += 1;
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_encrypted_playlist_is_refused_by_name() {
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
        let error = check(&media, &url).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("AES-128"), "{text}");
        assert!(text.contains("decryption"), "{text}");
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
