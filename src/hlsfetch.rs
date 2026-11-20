//! Turning a playlist into a file.
//!
//! `hls` reads the text and says what the segments are; `ts`, `mp4read` and
//! `mp4write` know what to do with the bytes. This module is the middle: it
//! fetches the segments, demuxes the container they carry, and hands the
//! samples to the writer. A playlist with
//! `EXT-X-MAP` is the fragmented-MP4 flavour: its header is fetched once and
//! every segment is parsed as `moof`+`mdat` against it.
//!
//! Transport-stream segments are demuxed as they arrive in playlist order.
//! Fragmented MP4 keeps the concatenated input for offsets across fragments.
//! In both cases the download window limits how many segment bodies are
//! waiting for an earlier request.
//!
//! Segments are cached beside the staging file. An interrupted list resumes
//! by replaying the cache and
//! asking the server only for the segments that are missing.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread;

use crate::error::{Error, Result};
use crate::hls::{Encryption, Key, Media, Playlist, Segment};
use crate::http::Http;
use crate::items::Items;
use crate::mp4read;
use crate::mp4write;
use crate::select::Quality;
use crate::ts::{self, Track, TrackKind};
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
    /// Segments of that count which came from the cache rather than the wire.
    pub resumed_segments: usize,
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

/// What the caller decides about *which* segments are taken and how many are in
/// flight: `--playlist-items` and `--concurrent-fragments` together, because
/// they are two answers to one question and eight positional arguments was one
/// too many.
#[derive(Debug, Clone, Copy)]
pub struct Segments<'a> {
    /// `--playlist-items`: positions into the list, 1-based, `None` for all.
    pub items: Option<&'a Items>,
    /// `--concurrent-fragments`: how many may be in flight. A `0` is treated as
    /// `1` here; the command line refuses it before a request is made.
    pub concurrent: usize,
    /// The staging file whose sidecars cache the segments between runs, or
    /// `None` when `--no-resume` said not to. The cache lives beside the file
    /// rather than in it: the file is an MP4 written once at the end, and a
    /// half-written one has no offset worth continuing from.
    pub resume: Option<&'a Path>,
    /// `--max-filesize`: no segment may be larger than this. `None` leaves the
    /// memory bound of `SEGMENT_LIMIT` as the only ceiling.
    pub max_bytes: Option<u64>,
}

impl<'a> Default for Segments<'a> {
    fn default() -> Segments<'a> {
        Segments {
            items: None,
            concurrent: 1,
            resume: None,
            max_bytes: None,
        }
    }
}

/// Fetches `url` and writes an MP4 at `out`.
///
/// `headers` are the site's request headers (a referrer, usually); every
/// request in the walk carries them. `items` is `--playlist-items`: positions
/// into the segment list, 1-based, or `None` for all of them. `concurrency` is
/// `segments` carries `--playlist-items` and `--concurrent-fragments`: which
/// segments of the list are taken, and how many of those may be in flight at
/// once, clamped to `1..=CONCURRENCY_LIMIT`. `on_bytes` is called with the size
/// of each segment as it lands — in whatever order they land, because it feeds
/// a byte counter, not a file — so the caller's progress bar moves while the
/// file does not exist yet.
pub fn fetch(
    http: &Http,
    url: &Url,
    headers: &[(String, String)],
    quality: Quality,
    segments: Segments<'_>,
    out: &Path,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<Report> {
    let items = segments.items;
    let resume = segments.resume;
    let concurrency = segments.concurrent.clamp(1, CONCURRENCY_LIMIT);
    // The ceiling a single segment is read under, settled once here rather than
    // recomputed per segment: it is the user's limit when there is one, and the
    // module's memory bound otherwise.
    let ceiling = segments.max_bytes.unwrap_or(SEGMENT_LIMIT);
    let borrowed: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();

    let (text, resolved) = http.get_text_at(url, &borrowed)?;
    let (video_media, video_url, audio_url, dimensions) = match Playlist::parse(&text, &resolved)? {
        Playlist::Media(media) => (media, resolved, None, (0, 0)),
        Playlist::Master(master) => {
            let variant = master.choose(quality).ok_or_else(|| {
                Error::Parse(format!("{resolved}: the master playlist names no variants"))
            })?;
            let audio = master
                .audio_for(variant)
                .and_then(|rendition| rendition.uri.clone());
            (
                media_playlist(http, &variant.uri, &borrowed)?,
                variant.uri.clone(),
                audio,
                (variant.width, variant.height),
            )
        }
    };
    check(&video_media, &video_url)?;
    let video_segments = selected(&video_media, items, &video_url)?;
    let mut segments = 0usize;
    // The init segment is fetched once, before the media segments: it is what
    // they are parsed against, and a header that cannot be read makes every
    // segment after it pointless.
    let video_map = init_section(http, &video_media, &borrowed, on_bytes)?;
    // The cache is opened before the first segment request: what a previous run
    // left is cheaper than what the server would send again.
    let mut video_cache = match resume {
        None => Cache::disabled(),
        Some(part) => Cache::open(
            &sidecar(part, ".segments"),
            &sidecar(part, ".journal"),
            &video_segments,
        ),
    };
    let resumed_video = video_cache.done;
    let mut tracks = fetch_tracks(
        http,
        &video_segments,
        &mut Walk {
            headers: &borrowed,
            concurrency,
            ceiling,
            on_bytes,
        },
        &mut segments,
        &mut video_cache,
        video_map.as_deref(),
    )?;
    let mut duration: f64 = video_segments.iter().map(|segment| segment.duration).sum();
    let video_has_audio = tracks.iter().any(|track| track.kind == TrackKind::Audio);
    let mut warnings = Vec::new();
    if let Some(warning) = video_cache.warning.take() {
        warnings.push(warning);
    }
    if items.is_some() {
        if let Some(first) = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .and_then(|track| track.samples.first())
        {
            if !first.keyframe {
                warnings.push(
                    "the selection does not begin on a keyframe: the file will not play from \
                     its first frame in every player"
                        .to_string(),
                );
            }
        }
    }

    // A variant with an audio group usually carries no audio of its own; when
    // it does, that audio is in sync with the video by construction, so the
    // separate rendition is not fetched over it.
    let mut resumed_audio = 0usize;
    if let Some(audio_url) = audio_url.filter(|_| !video_has_audio) {
        let audio_media = media_playlist(http, &audio_url, &borrowed)?;
        check(&audio_media, &audio_url)?;
        let audio_segments = selected(&audio_media, items, &audio_url)?;
        let audio_map = init_section(http, &audio_media, &borrowed, on_bytes)?;
        let mut audio_cache = match resume {
            None => Cache::disabled(),
            Some(part) => Cache::open(
                &sidecar(part, ".audio.segments"),
                &sidecar(part, ".audio.journal"),
                &audio_segments,
            ),
        };
        resumed_audio = audio_cache.done;
        let audio = fetch_tracks(
            http,
            &audio_segments,
            &mut Walk {
                headers: &borrowed,
                concurrency,
                ceiling,
                on_bytes,
            },
            &mut segments,
            &mut audio_cache,
            audio_map.as_deref(),
        )?;
        if let Some(warning) = audio_cache.warning.take() {
            warnings.push(warning);
        }
        match audio
            .into_iter()
            .find(|track| track.kind == TrackKind::Audio)
        {
            Some(track) => tracks.push(track),
            None => {
                return Err(Error::Parse(format!(
                    "{audio_url}: the audio rendition carries no audio track"
                )))
            }
        }
        duration = duration.max(audio_segments.iter().map(|segment| segment.duration).sum());
    }

    let report = mp4write::write(&tracks, dimensions, out)?;
    Ok(Report {
        bytes: report.bytes,
        segments,
        resumed_segments: resumed_video + resumed_audio,
        duration_ms: (duration * 1000.0).round() as u64,
        dimensions,
        video_samples: report.video_samples,
        audio_samples: report.audio_samples,
        warnings,
    })
}

/// Fetch and parse one segment list.
fn media_playlist(http: &Http, url: &Url, headers: &[(&str, &str)]) -> Result<Media> {
    let (text, resolved) = http.get_text_at(url, headers)?;
    match Playlist::parse(&text, &resolved)? {
        Playlist::Media(media) => Ok(media),
        Playlist::Master(_) => Err(Error::Parse(format!(
            "{url}: expected a list of segments, found a list of variants"
        ))),
    }
}

/// The gates between a playlist and a fetch.
fn check(media: &Media, url: &Url) -> Result<()> {
    // `EXT-X-MAP` names the header every following segment is parsed against.
    // One header per playlist is what this build can honour: a playlist that
    // swaps headers part way through describes two track sets, and a file made
    // of both would be neither.
    if let Some(first) = media
        .segments
        .iter()
        .find_map(|segment| segment.map.as_ref())
    {
        for (index, segment) in media.segments.iter().enumerate() {
            match segment.map.as_ref() {
                Some(map) if map != first => {
                    return Err(Error::Refused(format!(
                        "{url}: the playlist changes its EXT-X-MAP at segment {} ({} instead of \
                         {}), and this build stitches one header per playlist",
                        index + 1,
                        map.uri,
                        first.uri
                    )));
                }
                Some(_) => {}
                None => {
                    return Err(Error::Refused(format!(
                        "{url}: segment {} has no initialisation section although EXT-X-MAP was \
                         declared before it, so the segments are not one file",
                        index + 1
                    )));
                }
            }
        }
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
fn decrypt(mut buffer: Vec<u8>, key: [u8; KEY_BYTES], iv: [u8; 16]) -> Result<Vec<u8>> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

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

/// The `EXT-X-MAP` header, fetched once, with its byte range when it has one.
///
/// A playlist without one is the transport-stream flavour and returns `None`;
/// the caller then demuxes the bytes instead of parsing them as fragments.
fn init_section(
    http: &Http,
    media: &Media,
    headers: &[(&str, &str)],
    on_bytes: &mut dyn FnMut(u64),
) -> Result<Option<Vec<u8>>> {
    let map = match media
        .segments
        .iter()
        .find_map(|segment| segment.map.as_ref())
    {
        Some(map) => map,
        None => return Ok(None),
    };
    let range = map.range.map(|range| (range.offset, range.length));
    let bytes = http.get_bytes(
        "hls-init",
        &map.uri.to_string(),
        headers,
        range,
        SEGMENT_LIMIT,
    )?;
    on_bytes(bytes.len() as u64);
    Ok(Some(bytes))
}

/// Fetches and demuxes one rendition in playlist order.
fn fetch_tracks(
    http: &Http,
    chosen: &[&Segment],
    walk: &mut Walk<'_>,
    segments: &mut usize,
    cache: &mut Cache,
    init: Option<&[u8]>,
) -> Result<Vec<Track>> {
    if let Some(init) = init {
        let init = mp4read::init(init)?;
        // Some fragments use offsets relative to the concatenated rendition.
        let mut media = Vec::new();
        fetch_media(http, chosen, walk, segments, cache, &mut |bytes| {
            media.extend_from_slice(bytes);
            Ok(())
        })?;
        mp4read::fragments(&init, &media)
    } else {
        let mut demuxer = ts::Demuxer::default();
        fetch_media(http, chosen, walk, segments, cache, &mut |bytes| {
            demuxer.push(bytes)
        })?;
        Ok(demuxer.finish()?.tracks)
    }
}

/// One segment, fetched and — if the playlist says so — decrypted.
///
/// `ceiling` is `--max-filesize` when it is set, and `SEGMENT_LIMIT` otherwise.
/// No segment can be larger than the file it is a piece of, so applying the
/// user's ceiling here is exact rather than conservative: a segment past it is
/// a file that will be refused later, and refusing it before it is read is the
/// cheaper half of the same answer. Taking the smaller of the two keeps a
/// `--max-filesize 2G` from lifting the memory bound.
fn fetch_segment(
    http: &Http,
    segment: &Segment,
    headers: &[(&str, &str)],
    key: Option<([u8; KEY_BYTES], [u8; 16])>,
    ceiling: u64,
) -> Result<Vec<u8>> {
    let range = segment.range.map(|range| (range.offset, range.length));
    let limit = SEGMENT_LIMIT.min(ceiling.max(1));
    let mut bytes = http.get_bytes(
        "hls-segment",
        &segment.uri.to_string(),
        headers,
        range,
        limit,
    )?;
    if let Some((key_bytes, iv)) = key {
        bytes = decrypt(bytes, key_bytes, iv)?;
    }
    Ok(bytes)
}

/// Where the fetched segments wait between runs of the same download.
///
/// The staging file cannot be the cache: it is an MP4, written once at the end,
/// and a half-written one has no offset to continue from. What is resumable is
/// the segment stream, so the segments are kept beside it — the payload in
/// `<part>.segments`, one length per line in `<part>.journal`, and a header that
/// fingerprints the selection. A playlist that changed, an `--playlist-items`
/// that changed or a cache written by another build is a different download, and
/// the cache is dropped rather than spliced into a file it does not describe.
struct Cache {
    payload: Option<fs::File>,
    journal: Option<fs::File>,
    /// The whole prefix on disk, read once before asking for fresh segments.
    cached: Option<(fs::File, u64)>,
    /// How many whole segments the journal describes.
    done: usize,
    /// The first thing that went wrong writing the cache, said once. A cache is
    /// an optimisation: its failure is a note, not a failure of the download.
    warning: Option<String>,
}

impl Cache {
    /// A cache that stores nothing: `--no-resume`, or no staging file to put one
    /// beside.
    fn disabled() -> Cache {
        Cache {
            payload: None,
            journal: None,
            cached: None,
            done: 0,
            warning: None,
        }
    }

    /// Opens the cache for `chosen`, keeping whatever of it a previous run left.
    fn open(payload_path: &Path, journal_path: &Path, chosen: &[&Segment]) -> Cache {
        let header = header_line(chosen);
        let (file, lengths) = read_cache(payload_path, journal_path, &header, chosen.len())
            .map_or_else(
                || (None, Vec::new()),
                |(file, lengths)| (Some(file), lengths),
            );
        let kept: u64 = lengths.iter().sum();
        let cached = file.map(|file| (file, kept));
        match open_files(payload_path, journal_path, kept, &header, &lengths) {
            Ok((payload, journal)) => Cache {
                payload: Some(payload),
                journal: Some(journal),
                cached,
                done: lengths.len(),
                warning: None,
            },
            Err(error) => Cache {
                payload: None,
                journal: None,
                cached,
                done: lengths.len(),
                warning: Some(format!(
                    "the segment cache beside the staging file could not be opened ({error}); \
                     an interrupted run will start this download over"
                )),
            },
        }
    }

    fn replay(&mut self, consume: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<u64> {
        let Some((mut file, length)) = self.cached.take() else {
            return Ok(0);
        };
        let mut buffer = [0; 64 * 1024];
        let mut remaining = length;
        while remaining > 0 {
            let size = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..size])?;
            consume(&buffer[..size])?;
            remaining -= size as u64;
        }
        Ok(length)
    }

    /// Appends one segment, in playlist order.
    fn push(&mut self, bytes: &[u8]) {
        if let (Some(payload), Some(journal)) = (self.payload.as_mut(), self.journal.as_mut()) {
            if let Err(error) = write_segment(payload, journal, bytes) {
                if self.warning.is_none() {
                    self.warning = Some(format!(
                        "the segment cache could not be written ({error}); an interrupted run \
                         will start this download over"
                    ));
                }
                self.payload = None;
                self.journal = None;
            }
        }
        self.done += 1;
    }
}

/// `<part>.segments`, `<part>.journal`: sidecars, so the staging file keeps its
/// name and a stale cache cannot be mistaken for a file.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    crate::paths::verbatim(&PathBuf::from(name))
}

/// Deletes the caches beside a staging file: the download either finished (the
/// MP4 is what was wanted) or is being abandoned with `--no-resume`.
pub fn forget(part: &Path) {
    for suffix in [".segments", ".journal", ".audio.segments", ".audio.journal"] {
        let _ = fs::remove_file(sidecar(part, suffix));
    }
}

/// The line a cache starts with: this build's format, how many segments the
/// selection has, and a fingerprint of the segment URIs.
///
/// The fingerprint is FNV-1a over every URI with a separator between them. It is
/// not a checksum to trust bytes to; it is enough to notice that the playlists
/// are not the ones the cache describes.
fn header_line(chosen: &[&Segment]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for segment in chosen {
        for byte in segment.uri.to_string().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("# tafas segments 1 {} {hash:016x}", chosen.len())
}

/// What a previous run left: the whole segments, and their lengths.
///
/// A journal that is torn, a header that names another selection and a payload
/// shorter than its own journal all mean the same thing — take the prefix that
/// is whole and forget the rest, or forget all of it.
fn read_cache(
    payload_path: &Path,
    journal_path: &Path,
    header: &str,
    count: usize,
) -> Option<(fs::File, Vec<u64>)> {
    let journal = fs::read_to_string(journal_path).ok()?;
    let mut lines = journal.lines();
    if lines.next()? != header {
        return None;
    }
    let payload = fs::File::open(payload_path).ok()?;
    let available = payload.metadata().ok()?.len();
    let mut lengths = Vec::new();
    let mut total = 0u64;
    for line in lines.take(count) {
        // A line that does not parse is a write that was interrupted; the
        // segments before it are still whole.
        let Ok(length) = line.trim().parse::<u64>() else {
            break;
        };
        if total.saturating_add(length) > available {
            break;
        }
        total += length;
        lengths.push(length);
    }
    Some((payload, lengths))
}

/// Cuts the payload back to whole segments, rewrites the journal to match, and
/// opens both for appending.
fn open_files(
    payload_path: &Path,
    journal_path: &Path,
    kept: u64,
    header: &str,
    lengths: &[u64],
) -> std::io::Result<(fs::File, fs::File)> {
    // Bytes past the last whole segment belong to a segment that will be fetched
    // again, so they are cut rather than stitched onto its replacement.
    let payload = fs::OpenOptions::new()
        .create(true)
        .write(true)
        // Not truncating here: the length is set below, to the last whole
        // segment rather than to zero.
        .truncate(false)
        .open(payload_path)?;
    payload.set_len(kept)?;
    drop(payload);

    let mut journal = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(journal_path)?;
    writeln!(journal, "{header}")?;
    for length in lengths {
        writeln!(journal, "{length}")?;
    }
    journal.flush()?;
    drop(journal);

    Ok((
        fs::OpenOptions::new().append(true).open(payload_path)?,
        fs::OpenOptions::new().append(true).open(journal_path)?,
    ))
}

/// One segment into the cache: the payload first and synced, then its length.
///
/// The order is the whole point. A run that dies between the two leaves bytes
/// the journal does not claim, which recovery cuts; the other order would claim
/// bytes that never landed. A segment is seconds of video, so a sync per segment
/// costs nothing worth counting.
fn write_segment(
    payload: &mut fs::File,
    journal: &mut fs::File,
    bytes: &[u8],
) -> std::io::Result<()> {
    payload.write_all(bytes)?;
    payload.sync_data()?;
    writeln!(journal, "{}", bytes.len())?;
    journal.flush()
}

/// The answers every segment fetch in one run shares: how many workers, how
/// large a body may be, and where progress is reported.
///
/// Named rather than passed as four positional arguments because the two calls
/// that reach `fetch_media` — video, then audio — were eight arguments of which
/// six were identical, which is a shape where a transposed pair compiles and
/// lies. The compiler sees one argument now, and clippy's argument count agrees.
struct Walk<'a> {
    headers: &'a [(&'a str, &'a str)],
    concurrency: usize,
    ceiling: u64,
    on_bytes: &'a mut dyn FnMut(u64),
}

/// One list of segments, fetched in order into a cache that may already hold
/// the beginning of it.
///
/// `cache` is charged in playlist order as the pieces become contiguous, so a
/// run interrupted in the middle of a long list keeps what it had and asks for
/// the rest next time. The consumer sees the cached prefix and new segments
/// in playlist order.
fn fetch_media(
    http: &Http,
    chosen: &[&Segment],
    walk: &mut Walk<'_>,
    segments: &mut usize,
    cache: &mut Cache,
    consume: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let ceiling = walk.ceiling;
    let concurrency = walk.concurrency;
    // Reborrowed, not moved: the caller passes `&mut Walk`, and taking the
    // closure by value here would consume it on the first call and make the
    // audio walk a second borrow of something already gone.
    let on_bytes = &mut *walk.on_bytes;
    let headers = walk.headers;
    // Everything before `skip` is already in the cache: it was fetched by the
    // run this one is continuing.
    let skip = cache.done;
    if skip > 0 {
        on_bytes(cache.replay(consume)?);
    }

    // Keys are resolved before any worker starts, and only for the segments
    // that still have to be fetched: a key for a cached segment would be a
    // request for nothing.
    let mut key_cache: HashMap<String, [u8; KEY_BYTES]> = HashMap::new();
    let mut keys: Vec<Option<([u8; KEY_BYTES], [u8; 16])>> = vec![None; chosen.len()];
    for index in skip..chosen.len() {
        let segment = chosen[index];
        keys[index] = match segment
            .key
            .as_ref()
            .filter(|key| key.method == Encryption::Aes128)
        {
            // The IV belongs to the segment, not to the key block: a playlist
            // that omits `IV` expects the media sequence number, which differs
            // per segment. Both are settled here so the fetch path stays a
            // fetch.
            Some(key) => Some((
                resolve_key(http, key, headers, &mut key_cache)?,
                key.iv_for(segment.sequence),
            )),
            None => None,
        };
    }

    let remaining = chosen.len() - skip;
    let workers = concurrency.clamp(1, remaining.max(1));
    if workers == 1 {
        for index in skip..chosen.len() {
            let bytes = fetch_segment(http, chosen[index], headers, keys[index], ceiling)?;
            on_bytes(bytes.len() as u64);
            consume(&bytes)?;
            cache.push(&bytes);
        }
    } else {
        let cancelled = AtomicBool::new(false);
        let mut flushed = skip;
        let mut next = skip;
        let window = workers * 2;
        let mut pieces: Vec<Option<Vec<u8>>> = (0..chosen.len()).map(|_| None).collect();
        let (jobs, pending) = mpsc::channel();
        let pending = Mutex::new(pending);
        let (sender, receiver) = mpsc::channel();
        thread::scope(|scope| -> Result<()> {
            for _ in 0..workers {
                let sender = sender.clone();
                let pending = &pending;
                let cancelled = &cancelled;
                let keys = &keys;
                scope.spawn(move || {
                    while !cancelled.load(Ordering::Relaxed) {
                        let Ok(index) = pending.lock().expect("job queue").recv() else {
                            break;
                        };
                        let outcome =
                            fetch_segment(http, chosen[index], headers, keys[index], ceiling);
                        if outcome.is_err() {
                            cancelled.store(true, Ordering::Relaxed);
                        }
                        if sender.send((index, outcome)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(sender);
            let mut jobs = Some(jobs);
            let mut failure = None;
            loop {
                if let Some(queue) = &jobs {
                    while next < chosen.len() && next < flushed + window {
                        if queue.send(next).is_err() {
                            break;
                        }
                        next += 1;
                    }
                    if next == chosen.len() || cancelled.load(Ordering::Relaxed) {
                        jobs.take();
                    }
                }
                let Ok((index, outcome)) = receiver.recv() else {
                    break;
                };
                match outcome {
                    Ok(bytes) => {
                        on_bytes(bytes.len() as u64);
                        pieces[index] = Some(bytes);
                        while let Some(bytes) = pieces.get_mut(flushed).and_then(Option::take) {
                            if let Err(error) = consume(&bytes) {
                                cancelled.store(true, Ordering::Relaxed);
                                return Err(error);
                            }
                            cache.push(&bytes);
                            flushed += 1;
                        }
                    }
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            }
            failure.map_or(Ok(()), Err)
        })?;
    }

    *segments += chosen.len();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A playlist whose segments are named `seg1.ts`, `seg2.ts`, ...
    fn chosen_of(count: usize) -> (Playlist, Url) {
        let url = Url::parse("https://example.test/media.m3u8").unwrap();
        let mut text = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:6\n");
        for index in 1..=count {
            text.push_str(&format!("#EXTINF:6.0,\nseg{index}.ts\n"));
        }
        text.push_str("#EXT-X-ENDLIST\n");
        (Playlist::parse(&text, &url).unwrap(), url)
    }

    fn segments_of(playlist: &Playlist) -> Vec<&Segment> {
        match playlist {
            Playlist::Media(media) => media.segments.iter().collect(),
            Playlist::Master(_) => panic!("expected a segment list"),
        }
    }

    fn cache_paths(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("tafas-cache-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        (
            dir.join(format!("{name}.segments")),
            dir.join(format!("{name}.journal")),
        )
    }

    #[test]
    fn a_cache_keeps_the_whole_segments_it_was_given() {
        let (playlist, _) = chosen_of(3);
        let chosen = segments_of(&playlist);
        let (payload, journal) = cache_paths("round-trip");
        let _ = fs::remove_file(&payload);
        let _ = fs::remove_file(&journal);

        let mut cache = Cache::open(&payload, &journal, &chosen);
        assert_eq!(cache.done, 0, "a fresh cache holds nothing");
        cache.push(b"first");
        cache.push(b"second");
        assert_eq!(cache.done, 2);

        let mut reopened = Cache::open(&payload, &journal, &chosen);
        assert_eq!(reopened.done, 2, "two segments were whole");
        let mut data = Vec::new();
        assert_eq!(
            reopened
                .replay(&mut |bytes| {
                    data.extend_from_slice(bytes);
                    Ok(())
                })
                .unwrap(),
            11
        );
        assert_eq!(data, b"firstsecond");
        let _ = fs::remove_file(&payload);
        let _ = fs::remove_file(&journal);
    }

    #[test]
    fn a_cache_for_another_selection_is_not_reused() {
        let (playlist, _) = chosen_of(2);
        let chosen = segments_of(&playlist);
        let (other, _) = chosen_of(3);
        let other_chosen = segments_of(&other);
        let (payload, journal) = cache_paths("other-selection");
        let _ = fs::remove_file(&payload);
        let _ = fs::remove_file(&journal);

        let mut cache = Cache::open(&payload, &journal, &chosen);
        cache.push(b"first");
        drop(cache);

        let reopened = Cache::open(&payload, &journal, &other_chosen);
        assert_eq!(
            reopened.done, 0,
            "a list that is not the one the cache describes starts over"
        );
        let _ = fs::remove_file(&payload);
        let _ = fs::remove_file(&journal);
    }

    #[test]
    fn a_torn_tail_is_cut_back_to_whole_segments() {
        let (playlist, _) = chosen_of(2);
        let chosen = segments_of(&playlist);
        let (payload, journal) = cache_paths("torn");
        let _ = fs::remove_file(&payload);
        let _ = fs::remove_file(&journal);

        let mut cache = Cache::open(&payload, &journal, &chosen);
        cache.push(b"first");
        drop(cache);
        // A run that died in the middle of the second segment: bytes landed, the
        // journal never claimed them.
        let mut file = fs::OpenOptions::new().append(true).open(&payload).unwrap();
        file.write_all(b"half a seg").unwrap();
        drop(file);

        let mut reopened = Cache::open(&payload, &journal, &chosen);
        assert_eq!(reopened.done, 1);
        let mut data = Vec::new();
        reopened
            .replay(&mut |bytes| {
                data.extend_from_slice(bytes);
                Ok(())
            })
            .unwrap();
        assert_eq!(data, b"first", "the tail belongs to a re-fetch");
        let _ = fs::remove_file(&payload);
        let _ = fs::remove_file(&journal);
    }

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
        let error = decrypt(vec![0x42; 17], [0u8; 16], [0u8; 16]).unwrap_err();
        assert!(error.to_string().contains("did not decrypt"), "{error}");
    }

    #[test]
    fn an_fmp4_playlist_passes_the_gate() {
        let url = Url::parse("https://example.test/media.m3u8").unwrap();
        let text = "#EXTM3U\n\
                    #EXT-X-MAP:URI=\"init.mp4\"\n\
                    #EXTINF:6.0,\n\
                    seg1.m4s\n\
                    #EXTINF:6.0,\n\
                    seg2.m4s\n\
                    #EXT-X-ENDLIST\n";
        let media = match Playlist::parse(text, &url).unwrap() {
            Playlist::Media(media) => media,
            Playlist::Master(_) => panic!("expected a segment list"),
        };
        assert!(
            check(&media, &url).is_ok(),
            "EXT-X-MAP is a header to read, not a refusal"
        );
    }

    #[test]
    fn a_playlist_that_swaps_its_header_is_refused() {
        let url = Url::parse("https://example.test/media.m3u8").unwrap();
        let text = "#EXTM3U\n\
                    #EXT-X-MAP:URI=\"init.mp4\"\n\
                    #EXTINF:6.0,\n\
                    seg1.m4s\n\
                    #EXT-X-MAP:URI=\"other.mp4\"\n\
                    #EXTINF:6.0,\n\
                    seg2.m4s\n\
                    #EXT-X-ENDLIST\n";
        let media = match Playlist::parse(text, &url).unwrap() {
            Playlist::Media(media) => media,
            Playlist::Master(_) => panic!("expected a segment list"),
        };
        let error = check(&media, &url).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("EXT-X-MAP"), "{message}");
        assert!(message.contains("segment 2"), "{message}");
        assert!(message.contains("init.mp4"), "{message}");
    }

    #[test]
    fn the_two_containers_are_told_apart_by_the_playlist() {
        // The header decides, not a sniff of the bytes: a fragment with no
        // `EXT-X-MAP` in front of it is read as a transport stream, and the
        // error says so instead of guessing.
        let error = ts::demux(b"\x00moof").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("transport stream"), "{text}");
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
