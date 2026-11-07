//! HTTP Live Streaming: the playlist format, and the rules that decide whether
//! a playlist is one this build can turn into a file.
//!
//! Only the parts a downloader needs are read: where the segments are, how long
//! each one is, which byte range of which file it occupies, what it is
//! encrypted with, and whether the recording has finished. Everything else in
//! the specification — variant selection metadata beyond height and bandwidth,
//! `EXT-X-I-FRAME-STREAM-INF`, gap markers — is skipped on purpose, because a
//! tag this program ignores is a tag it cannot get wrong.
//!
//! Two rules are enforced here rather than later, because they decide whether
//! the rest of the pipeline has anything to do:
//!
//! * a playlist without `EXT-X-ENDLIST` is still growing, so there is no
//!   finished recording to write — that is a refusal with a reason,
//!   not a truncated file;
//! * sample-level encryption (`METHOD=SAMPLE-AES`) is refused at parse time:
//!   it hides the picture inside the container's own framing, and a partial
//!   implementation would produce a file that plays for a second and then stops.

use crate::error::{Error, Result};
use crate::select::Quality;
use crate::url::Url;

/// A parsed playlist: either a list of variants, or a list of segments.
#[derive(Debug, Clone, PartialEq)]
pub enum Playlist {
    Master(Master),
    Media(Media),
}

impl Playlist {
    pub fn parse(text: &str, base: &Url) -> Result<Playlist> {
        parse(text, base)
    }
}

/// A master playlist: what to fetch, and what it is worth.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Master {
    pub variants: Vec<Variant>,
    /// `EXT-X-MEDIA` renditions. Only audio is modelled; subtitles are read so
    /// that a playlist naming them is not mistaken for one without them.
    pub renditions: Vec<Rendition>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub uri: Url,
    pub bandwidth: u64,
    pub width: u32,
    pub height: u32,
    pub codecs: Option<String>,
    /// The `EXT-X-MEDIA` group whose audio belongs with this variant, if any.
    pub audio_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rendition {
    pub kind: RenditionKind,
    pub group: String,
    pub uri: Option<Url>,
    pub language: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenditionKind {
    Audio,
    Subtitles,
}

/// A media playlist: the segments, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub segments: Vec<Segment>,
    pub target_duration: u64,
    pub end_list: bool,
    pub playlist_type: PlaylistType,
}

impl Media {
    /// The longest segment, as the container should declare it. `EXTINF` is
    /// rounded in the playlist, so the largest value is the honest bound.
    pub fn longest_segment(&self) -> f64 {
        self.segments
            .iter()
            .fold(0.0_f64, |longest, segment| longest.max(segment.duration))
    }

    /// The encryption this playlist asks for, if all segments agree.
    pub fn encryption(&self) -> Option<Encryption> {
        self.segments
            .iter()
            .filter_map(|segment| segment.key.as_ref().map(|key| key.method))
            .find(|method| *method != Encryption::None)
    }

    pub fn total_duration(&self) -> f64 {
        self.segments.iter().map(|segment| segment.duration).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistType {
    Vod,
    Event,
    /// No `EXT-X-PLAYLIST-TYPE` and no `EXT-X-ENDLIST`: a live playlist.
    Live,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub uri: Url,
    pub duration: f64,
    /// `EXT-X-MEDIA-SEQUENCE` plus the index, which is what an implicit AES-128
    /// IV is derived from.
    pub sequence: u64,
    pub range: Option<ByteRange>,
    pub key: Option<Key>,
    pub map: Option<InitSection>,
    pub discontinuity: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub offset: u64,
    pub length: u64,
}

impl ByteRange {
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.length)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InitSection {
    pub uri: Url,
    pub range: Option<ByteRange>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub method: Encryption,
    pub uri: Option<Url>,
    pub iv: Option<[u8; 16]>,
}

impl Key {
    /// The IV a segment is decrypted with. When the playlist does not name one,
    /// the specification says to use the segment's media sequence number as a
    /// 128-bit big-endian counter — a rule that is easy to miss and produces
    /// unplayable audio when it is.
    pub fn iv_for(&self, sequence: u64) -> [u8; 16] {
        match self.iv {
            Some(iv) => iv,
            None => {
                let mut iv = [0u8; 16];
                iv[8..].copy_from_slice(&sequence.to_be_bytes());
                iv
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// `METHOD=NONE`.
    None,
    /// `METHOD=AES-128`: whole segments, AES-128 in CBC mode.
    Aes128,
    /// `METHOD=SAMPLE-AES`: refused at parse time.
    SampleAes,
    /// A method this build has never heard of.
    Other,
}

impl Master {
    /// The variant to fetch, by the same rule the progressive picker uses: a
    /// height is a ceiling, and a playlist with nothing that small gives the
    /// smallest it has rather than an error the user cannot act on.
    pub fn choose(&self, quality: Quality) -> Option<&Variant> {
        if self.variants.is_empty() {
            return None;
        }
        let by_bandwidth = |variant: &&Variant| (variant.bandwidth, variant.height);
        match quality {
            Quality::Best => self
                .variants
                .iter()
                .filter(|variant| variant.height > 0)
                .max_by_key(by_bandwidth)
                .or_else(|| self.variants.iter().max_by_key(by_bandwidth)),
            Quality::Worst => self
                .variants
                .iter()
                .filter(|variant| variant.height > 0)
                .min_by_key(by_bandwidth)
                .or_else(|| self.variants.iter().min_by_key(by_bandwidth)),
            Quality::Height(height) => self
                .variants
                .iter()
                .filter(|variant| variant.height > 0 && variant.height <= height)
                .max_by_key(by_bandwidth)
                .or_else(|| {
                    // Nothing that small: the closest above it, so the request
                    // still produces a file.
                    self.variants
                        .iter()
                        .filter(|variant| variant.height > 0)
                        .min_by_key(by_bandwidth)
                }),
        }
    }

    /// The audio rendition that belongs with a variant, if the playlist names
    /// one and it has a URI of its own.
    pub fn audio_for(&self, variant: &Variant) -> Option<&Rendition> {
        let group = variant.audio_group.as_deref()?;
        self.renditions
            .iter()
            .filter(|rendition| {
                rendition.kind == RenditionKind::Audio
                    && rendition.group == group
                    && rendition.uri.is_some()
            })
            .max_by_key(|rendition| rendition.is_default)
    }
}

/// The `EXT-X-STREAM-INF` attributes that a variant needs, held between the tag
/// and the URI line that follows it.
struct VariantAttributes {
    bandwidth: u64,
    width: u32,
    height: u32,
    codecs: Option<String>,
    audio_group: Option<String>,
}

fn parse(text: &str, base: &Url) -> Result<Playlist> {
    let mut saw_header = false;

    let mut variants: Vec<Variant> = Vec::new();
    let mut renditions: Vec<Rendition> = Vec::new();
    // The attribute set of the last `EXT-X-STREAM-INF`, waiting for its URI.
    let mut pending_variant: Option<VariantAttributes> = None;

    let mut segments: Vec<Segment> = Vec::new();
    let mut target_duration = 0u64;
    let mut end_list = false;
    let mut playlist_type: Option<PlaylistType> = None;
    let mut current_map: Option<InitSection> = None;
    let mut current_key: Option<Key> = None;
    let mut pending_duration: Option<f64> = None;
    let mut pending_range: Option<(Option<u64>, u64)> = None;
    let mut pending_discontinuity = false;
    let mut next_sequence = 0u64;
    // Where the previous `EXT-X-BYTERANGE` ended, for the short form that only
    // names a length.
    let mut last_range_end: Option<u64> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if !saw_header {
            if line != "#EXTM3U" {
                return Err(Error::Parse(
                    "playlist does not start with #EXTM3U, so it is not an HLS playlist".into(),
                ));
            }
            saw_header = true;
            continue;
        }

        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attributes = attributes(rest);
            let (width, height) = resolution(&attributes);
            pending_variant = Some(VariantAttributes {
                bandwidth: number(&attributes, "BANDWIDTH").unwrap_or(0),
                width,
                height,
                codecs: value(&attributes, "CODECS").map(str::to_string),
                audio_group: value(&attributes, "AUDIO").map(str::to_string),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attributes = attributes(rest);
            let kind = match value(&attributes, "TYPE") {
                Some("AUDIO") => Some(RenditionKind::Audio),
                Some("SUBTITLES") => Some(RenditionKind::Subtitles),
                _ => None,
            };
            if let Some(kind) = kind {
                let group = value(&attributes, "GROUP-ID")
                    .unwrap_or_default()
                    .to_string();
                let uri = match value(&attributes, "URI") {
                    Some(uri) => Some(base.join(uri)?),
                    None => None,
                };
                renditions.push(Rendition {
                    kind,
                    group,
                    uri,
                    language: value(&attributes, "LANGUAGE").map(str::to_string),
                    is_default: value(&attributes, "DEFAULT").is_some_and(|flag| flag == "YES"),
                });
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let attributes = attributes(rest);
            let method = match value(&attributes, "METHOD") {
                Some("NONE") | None => Encryption::None,
                Some("AES-128") => Encryption::Aes128,
                Some(method) if method.eq_ignore_ascii_case("SAMPLE-AES") => Encryption::SampleAes,
                Some(_) => Encryption::Other,
            };
            match method {
                Encryption::None => current_key = None,
                Encryption::SampleAes => {
                    return Err(Error::Unsupported(
                        "this playlist uses SAMPLE-AES encryption, which hides the picture inside \
                         the container's own framing; tafas does not decrypt it"
                            .into(),
                    ))
                }
                Encryption::Other => {
                    return Err(Error::Unsupported(
                        "this playlist uses an encryption method tafas does not know".into(),
                    ))
                }
                Encryption::Aes128 => {
                    let uri = match value(&attributes, "URI") {
                        Some(uri) => base.join(uri)?,
                        None => return Err(Error::Parse("AES-128 key block without a URI".into())),
                    };
                    let iv = match value(&attributes, "IV") {
                        Some(iv) => Some(parse_iv(iv)?),
                        None => None,
                    };
                    let prefers_identity = value(&attributes, "KEYFORMAT")
                        .map(|format| format.eq_ignore_ascii_case("identity"))
                        .unwrap_or(true);
                    if !prefers_identity {
                        return Err(Error::Unsupported(format!(
                            "this playlist wraps its key in '{}', which tafas does not unwrap",
                            value(&attributes, "KEYFORMAT").unwrap_or("an unknown format")
                        )));
                    }
                    current_key = Some(Key {
                        method,
                        uri: Some(uri),
                        iv,
                    });
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let attributes = attributes(rest);
            let uri = match value(&attributes, "URI") {
                Some(uri) => base.join(uri)?,
                None => {
                    return Err(Error::Parse(
                        "EXT-X-MAP without a URI: the initialisation segment is unreadable".into(),
                    ))
                }
            };
            let range = match value(&attributes, "BYTERANGE") {
                Some(range) => Some(byte_range(range)?),
                None => None,
            };
            let (offset, length) = match range {
                Some((offset, length)) => (offset.unwrap_or(0), length),
                None => (0, 0),
            };
            current_map = Some(InitSection {
                uri,
                range: if length > 0 {
                    Some(ByteRange { offset, length })
                } else {
                    None
                },
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = Some(byte_range(rest)?);
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            // `#EXTINF:9.009,title`
            let value = rest.split(',').next().unwrap_or(rest).trim();
            pending_duration =
                Some(value.parse::<f64>().map_err(|_| {
                    Error::Parse(format!("EXTINF duration '{value}' is not a number"))
                })?);
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_duration = parse_u64(rest)?;
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            next_sequence = parse_u64(rest)?;
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            playlist_type = Some(match rest.trim() {
                "VOD" => PlaylistType::Vod,
                "EVENT" => PlaylistType::Event,
                _ => PlaylistType::Live,
            });
            continue;
        }
        if line == "#EXT-X-ENDLIST" {
            end_list = true;
            continue;
        }
        if line == "#EXT-X-DISCONTINUITY" {
            pending_discontinuity = true;
            continue;
        }
        if line.starts_with('#') {
            // A comment, or a tag this build does not need.
            continue;
        }

        // A URI line: either a variant or a segment, depending on what came
        // before it.
        let uri = base.join(line)?;
        if let Some(pending) = pending_variant.take() {
            variants.push(Variant {
                uri,
                bandwidth: pending.bandwidth,
                width: pending.width,
                height: pending.height,
                codecs: pending.codecs,
                audio_group: pending.audio_group,
            });
            continue;
        }
        let duration = pending_duration.take().unwrap_or(0.0);
        let range = match pending_range.take() {
            Some((Some(offset), length)) => {
                last_range_end = Some(offset.saturating_add(length));
                Some(ByteRange { offset, length })
            }
            Some((None, length)) => {
                let offset = last_range_end.ok_or_else(|| {
                    Error::Parse(
                        "EXT-X-BYTERANGE names a length with no offset and there is no previous \
                         sub-range to continue"
                            .into(),
                    )
                })?;
                last_range_end = Some(offset.saturating_add(length));
                Some(ByteRange { offset, length })
            }
            None => {
                last_range_end = None;
                None
            }
        };
        segments.push(Segment {
            uri,
            duration,
            sequence: next_sequence,
            range,
            key: current_key.clone(),
            map: current_map.clone(),
            discontinuity: pending_discontinuity,
        });
        next_sequence += 1;
        pending_discontinuity = false;
    }

    if !saw_header {
        return Err(Error::Parse("empty playlist: no #EXTM3U header".into()));
    }
    if !variants.is_empty() {
        return Ok(Playlist::Master(Master {
            variants,
            renditions,
        }));
    }
    if segments.is_empty() {
        return Err(Error::Parse(
            "playlist has neither segments nor variants".into(),
        ));
    }

    let playlist_type = playlist_type.unwrap_or(if end_list {
        PlaylistType::Vod
    } else {
        PlaylistType::Live
    });
    if !end_list && playlist_type != PlaylistType::Vod {
        return Err(Error::Unsupported(
            "this is a live HLS playlist: it has no #EXT-X-ENDLIST, so there is no finished \
             recording to write. Start the capture after the stream ends"
                .into(),
        ));
    }

    Ok(Playlist::Media(Media {
        segments,
        target_duration,
        end_list,
        playlist_type,
    }))
}

/// Splits an attribute list, respecting the quoted values that contain commas.
fn attributes(list: &str) -> Vec<(String, String)> {
    let bytes = list.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] == b',' || bytes[i] == b' ') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && bytes[i] != b',' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            break;
        }
        let key = list[key_start..i].trim().to_string();
        i += 1;
        let value = if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            let value = list[start..i].to_string();
            if i < bytes.len() {
                i += 1;
            }
            value
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b',' {
                i += 1;
            }
            list[start..i].trim().to_string()
        };
        out.push((key, value));
    }
    out
}

fn value<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn number(attributes: &[(String, String)], name: &str) -> Option<u64> {
    value(attributes, name).and_then(|value| value.trim().parse().ok())
}

fn resolution(attributes: &[(String, String)]) -> (u32, u32) {
    let parsed = value(attributes, "RESOLUTION").and_then(|value| {
        let (width, height) = value.split_once('x')?;
        Some((
            width.trim().parse::<u32>().ok()?,
            height.trim().parse::<u32>().ok()?,
        ))
    });
    parsed.unwrap_or((0, 0))
}

/// `EXT-X-BYTERANGE` and `EXT-X-MAP:BYTERANGE` both say `<length>[@<offset>]`.
fn byte_range(text: &str) -> Result<(Option<u64>, u64)> {
    let text = text.trim().trim_matches('"');
    match text.split_once('@') {
        Some((length, offset)) => Ok((Some(parse_u64(offset)?), parse_u64(length)?)),
        None => Ok((None, parse_u64(text)?)),
    }
}

fn parse_u64(text: &str) -> Result<u64> {
    text.trim()
        .trim_matches('"')
        .parse()
        .map_err(|_| Error::Parse(format!("playlist value '{text}' is not a number")))
}

fn parse_iv(text: &str) -> Result<[u8; 16]> {
    let hex = text
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if hex.len() != 32 {
        return Err(Error::Parse(format!(
            "IV '{text}' is not 128 bits of hexadecimal"
        )));
    }
    let mut iv = [0u8; 16];
    for (index, byte) in iv.iter_mut().enumerate() {
        let pair = &hex[index * 2..index * 2 + 2];
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| Error::Parse(format!("IV '{text}' is not hexadecimal")))?;
    }
    Ok(iv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example.com/vod/index.m3u8?token=abc").unwrap()
    }

    const VOD: &str = "\
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:7
#EXTINF:9.009,
seg1.ts
#EXTINF:9.009,
seg2.ts
#EXTINF:3.003,
seg3.ts
#EXT-X-ENDLIST
";

    #[test]
    fn reads_a_finished_playlist() {
        let Playlist::Media(media) = Playlist::parse(VOD, &base()).unwrap() else {
            panic!("a segment list is a media playlist");
        };
        assert_eq!(media.segments.len(), 3);
        assert_eq!(media.target_duration, 10);
        assert!(media.end_list);
        assert_eq!(media.playlist_type, PlaylistType::Vod);
        assert_eq!(media.segments[0].sequence, 7);
        assert_eq!(media.segments[2].sequence, 9);
        assert_eq!(
            media.segments[1].uri.to_string(),
            "https://cdn.example.com/vod/seg2.ts"
        );
        assert!((media.total_duration() - 21.021).abs() < 1e-9);
        assert_eq!(media.encryption(), None);
    }

    #[test]
    fn a_playlist_without_an_end_list_is_refused_by_name() {
        let live = VOD.replace("#EXT-X-ENDLIST\n", "");
        let error = Playlist::parse(&live, &base()).unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)));
        assert!(error.to_string().contains("EXT-X-ENDLIST"), "{error}");
    }

    #[test]
    fn a_playlist_that_does_not_start_with_the_header_is_a_parse_error() {
        let error = Playlist::parse("#EXT-X-TARGETDURATION:10\nseg.ts\n", &base()).unwrap_err();
        assert!(error.is_parse(), "{error}");
    }

    #[test]
    fn reads_a_master_playlist_and_chooses_by_height() {
        let master = "\
#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,URI=\"audio/en.m3u8\"
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\"
low/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\",AUDIO=\"aud\"
high/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720
mid/index.m3u8
";
        let Playlist::Master(master) = Playlist::parse(master, &base()).unwrap() else {
            panic!("a variant list is a master playlist");
        };
        assert_eq!(master.variants.len(), 3);
        assert_eq!(master.renditions.len(), 1);
        assert_eq!(master.variants[0].height, 360);

        let best = master.choose(Quality::Best).unwrap();
        assert_eq!(best.height, 1080);
        assert_eq!(best.audio_group.as_deref(), Some("aud"));
        assert_eq!(
            best.uri.to_string(),
            "https://cdn.example.com/vod/high/index.m3u8"
        );

        let capped = master.choose(Quality::Height(720)).unwrap();
        assert_eq!(capped.height, 720);

        let tiny = master.choose(Quality::Height(240)).unwrap();
        assert_eq!(tiny.height, 360, "the smallest available is used");

        assert_eq!(master.choose(Quality::Worst).unwrap().height, 360);

        let audio = master.audio_for(best).unwrap();
        assert_eq!(
            audio.uri.as_ref().unwrap().to_string(),
            "https://cdn.example.com/vod/audio/en.m3u8"
        );
        assert_eq!(audio.language.as_deref(), Some("en"));
    }

    #[test]
    fn reads_keys_maps_and_byte_ranges() {
        let playlist = "\
#EXTM3U
#EXT-X-TARGETDURATION:6
#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"720@0\"
#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090a0b0c0d0e0f
#EXTINF:5.0,
#EXT-X-BYTERANGE:1000@720
main.mp4
#EXTINF:5.0,
#EXT-X-BYTERANGE:2000
main.mp4
#EXT-X-DISCONTINUITY
#EXTINF:5.0,
other.ts
#EXT-X-ENDLIST
";
        let Playlist::Media(media) = Playlist::parse(playlist, &base()).unwrap() else {
            panic!("expected a media playlist");
        };
        assert_eq!(media.segments.len(), 3);

        let first = &media.segments[0];
        assert_eq!(
            first.map.as_ref().unwrap().range,
            Some(ByteRange {
                offset: 0,
                length: 720
            })
        );
        assert_eq!(
            first.range,
            Some(ByteRange {
                offset: 720,
                length: 1000
            })
        );
        let key = first.key.as_ref().unwrap();
        assert_eq!(key.method, Encryption::Aes128);
        assert_eq!(
            key.uri.as_ref().unwrap().to_string(),
            "https://cdn.example.com/vod/key.bin"
        );
        assert_eq!(key.iv.unwrap()[0], 0x00);
        assert_eq!(key.iv.unwrap()[15], 0x0f);

        // The second sub-range continues where the first ended.
        assert_eq!(
            media.segments[1].range,
            Some(ByteRange {
                offset: 1720,
                length: 2000
            })
        );
        assert!(!media.segments[1].discontinuity);
        assert!(media.segments[2].discontinuity);
        assert_eq!(media.encryption(), Some(Encryption::Aes128));
    }

    #[test]
    fn an_implicit_iv_is_the_media_sequence_number() {
        let key = Key {
            method: Encryption::Aes128,
            uri: None,
            iv: None,
        };
        let iv = key.iv_for(258);
        assert_eq!(&iv[..8], &[0u8; 8]);
        assert_eq!(&iv[14..], &[0x01, 0x02]);
    }

    #[test]
    fn sample_aes_is_refused_with_its_reason() {
        let playlist = "\
#EXTM3U
#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://key\"
#EXTINF:4.0,
seg.ts
#EXT-X-ENDLIST
";
        let error = Playlist::parse(playlist, &base()).unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)));
        assert!(error.to_string().contains("SAMPLE-AES"), "{error}");
    }

    #[test]
    fn a_byte_range_without_a_preceding_range_is_refused() {
        let playlist = "\
#EXTM3U
#EXTINF:4.0,
#EXT-X-BYTERANGE:100
main.mp4
#EXT-X-ENDLIST
";
        let error = Playlist::parse(playlist, &base()).unwrap_err();
        assert!(error.is_parse(), "{error}");
    }

    #[test]
    fn quoted_commas_do_not_split_an_attribute_list() {
        let attributes = attributes("BANDWIDTH=1,CODECS=\"avc1.4d401e,mp4a.40.2\",RESOLUTION=1x1");
        assert_eq!(attributes.len(), 3);
        assert_eq!(value(&attributes, "CODECS"), Some("avc1.4d401e,mp4a.40.2"));
        assert_eq!(value(&attributes, "RESOLUTION"), Some("1x1"));
    }
}
