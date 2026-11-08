use std::fmt;

/// The seven sites this build knows how to read.
///
/// A site is a *reader*, not a guarantee: `Site::from_url` answering `Some`
/// only means the URL names the site, not that the page can be captured. The
/// extractor still has to succeed, and refusals are a first-class outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    YouTube,
    TikTok,
    X,
    Vimeo,
    Reddit,
    SoundCloud,
    Twitch,
}

impl Site {
    pub fn as_str(self) -> &'static str {
        match self {
            Site::YouTube => "youtube",
            Site::TikTok => "tiktok",
            Site::X => "x",
            Site::Vimeo => "vimeo",
            Site::Reddit => "reddit",
            Site::SoundCloud => "soundcloud",
            Site::Twitch => "twitch",
        }
    }

    pub fn from_url(url: &crate::url::Url) -> Option<Site> {
        let host = url.host()?;
        let host = host.strip_prefix("m.").unwrap_or(host);
        let host = host.strip_prefix("mobile.").unwrap_or(host);
        match host {
            "youtube.com" | "youtu.be" | "music.youtube.com" | "youtube-nocookie.com" => {
                Some(Site::YouTube)
            }
            "tiktok.com" | "vt.tiktok.com" | "vm.tiktok.com" | "m.tiktok.com" => Some(Site::TikTok),
            "x.com" | "twitter.com" | "t.co" | "mobile.twitter.com" => Some(Site::X),
            "vimeo.com" | "player.vimeo.com" => Some(Site::Vimeo),
            "reddit.com" | "old.reddit.com" | "redd.it" | "v.redd.it" => Some(Site::Reddit),
            "soundcloud.com" | "on.soundcloud.com" => Some(Site::SoundCloud),
            "twitch.tv" => Some(Site::Twitch),
            _ => None,
        }
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a stream carries. A DASH pair is two streams that only become a video
/// once they are muxed; a progressive stream is already both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// Video and audio in one stream; nothing has to be muxed.
    Muxed,
    /// Video only.
    Video,
    /// Audio only.
    Audio,
}

impl StreamKind {
    pub fn has_video(self) -> bool {
        matches!(self, StreamKind::Muxed | StreamKind::Video)
    }

    pub fn has_audio(self) -> bool {
        matches!(self, StreamKind::Muxed | StreamKind::Audio)
    }
}

/// The container a stream arrives in. Only the ones tafas can write without
/// re-encoding appear here; anything else is dropped at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Webm,
    /// MPEG-TS, which is what an HLS segment is. Recognised so that the refusal
    /// can name it.
    Ts,
    /// An HLS playlist. The segments inside it are MPEG-TS and become an MP4,
    /// so this is a container that has to be *assembled* rather than written
    /// through: it is the only one that changes shape on the way to disk.
    Hls,
    Jpeg,
    /// MP3, which is written verbatim: there is no container to rebuild and
    /// nothing to verify beyond the byte count.
    Mp3,
    Unknown,
}

impl Container {
    pub fn extension(self) -> &'static str {
        match self {
            Container::Mp4 | Container::Ts | Container::Hls => "mp4",
            Container::Webm => "webm",
            Container::Jpeg => "jpg",
            Container::Mp3 => "mp3",
            Container::Unknown => "bin",
        }
    }

    /// The name printed in `--list-formats` and `--write-info-json`.
    pub fn name(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Webm => "webm",
            Container::Ts => "ts",
            Container::Hls => "hls",
            Container::Jpeg => "jpg",
            Container::Mp3 => "mp3",
            Container::Unknown => "bin",
        }
    }

    /// Whether a stream of this container can be remuxed by `mux::mp4`.
    pub fn is_isobmff(self) -> bool {
        matches!(self, Container::Mp4)
    }
}

/// One selectable stream, as the site described it.
#[derive(Debug, Clone)]
pub struct Format {
    /// The site's own identifier: a YouTube `itag` as a string, an X variant
    /// label, a TikTok quality label. Stable within a page, opaque across days.
    pub id: String,
    pub kind: StreamKind,
    pub container: Container,
    /// `avc1.640028`, `av01.0.08M.08`, `mp4a.40.2`, `opus`. Used for preference,
    /// never for parsing decisions.
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Bits per second, as advertised.
    pub bitrate: u64,
    /// Size in bytes when the site states it. `None` is normal for X variants.
    pub bytes: Option<u64>,
    /// A URL that can be fetched as-is. `None` means the site handed out a
    /// cipher instead, which this build refuses rather than guesses at.
    pub url: Option<String>,
    /// Headers the CDN expects. Kept per format because they differ by site.
    pub headers: Vec<(String, String)>,
    /// Which request produced this format, for `--list-formats` and `-v`.
    pub source: &'static str,
}

impl Format {
    pub fn label(&self) -> String {
        if self.height > 0 {
            format!(
                "{}p{}",
                self.height,
                if self.fps > 30 { self.fps } else { 0 }
            )
            .replace("p0", "p")
        } else {
            "audio".to_string()
        }
    }

    /// Rough size when the site did not state one: bitrate × duration. It is
    /// only ever used to print an estimate, never to decide anything.
    pub fn estimate_bytes(&self, seconds: u64) -> Option<u64> {
        if let Some(bytes) = self.bytes {
            return Some(bytes);
        }
        if self.bitrate == 0 || seconds == 0 {
            return None;
        }
        Some(self.bitrate / 8 * seconds)
    }
}

/// A still image attached to a post. X serves these next to video, and a post
/// with only photos is still worth capturing.
#[derive(Debug, Clone)]
pub struct Image {
    pub url: String,
    pub extension: &'static str,
    pub headers: Vec<(String, String)>,
}

/// One caption track the page offers.
///
/// A track is a *reference*, not a document: the cues are fetched only when
/// `--write-subs` asks for them, and the site's language code is what
/// `--sub-langs` matches against.
#[derive(Debug, Clone)]
pub struct Subtitle {
    /// The site's own code: `en`, `en-US`, `ru`.
    pub language: String,
    /// What the site calls the track, when it says: "English (auto-generated)".
    pub name: Option<String>,
    /// True when the site produced the cues from the audio rather than a person
    /// writing them. The file name says which, because they are not the same
    /// thing and one of them is usually wrong.
    pub automatic: bool,
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl Subtitle {
    /// `<stem>.<language>.vtt`, or `<stem>.<language>.auto.vtt` for a
    /// machine-transcribed track.
    pub fn file_name(&self, stem: &str) -> String {
        let language = language_code(&self.language);
        if self.automatic {
            format!("{stem}.{language}.auto.vtt")
        } else {
            format!("{stem}.{language}.vtt")
        }
    }
}

/// A language code, reduced to what a file name can hold. Codes are ASCII
/// (`en`, `pt-BR`, `zh-Hans`) but they come from a page, so the shape is
/// enforced rather than assumed.
pub fn language_code(language: &str) -> String {
    let cleaned: String = language
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "und".to_string()
    } else {
        cleaned
    }
}

/// Everything the extractor learned about one page, before any stream is
/// fetched.
#[derive(Debug, Clone)]
pub struct Media {
    pub site: Site,
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    /// Seconds, when known.
    pub duration: Option<u64>,
    pub formats: Vec<Format>,
    pub images: Vec<Image>,
    /// The poster frame, when the page names one. Written only on request
    /// (`--write-thumbnail`); nothing here fetches it otherwise.
    pub thumbnail: Option<Image>,
    /// The caption tracks the page lists. Empty means the page offers none,
    /// which is a fact worth reporting rather than an error.
    pub subtitles: Vec<Subtitle>,
    pub page_url: String,
}

/// One page that names other videos: a playlist, a channel, an album, a
/// profile.
///
/// It is not a [`Media`]: there is no stream to fetch and no single title to
/// write, only a list of entries. The reader returns it, and the download layer
/// decides what to do with each entry — which is why `--playlist-items` means
/// the same thing here as it does for an HLS segment list.
#[derive(Debug, Clone)]
pub struct Listing {
    pub site: Site,
    /// The playlist id, the channel handle, whatever names this page.
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    /// The entries, in the order the page lists them, before any selection.
    pub entries: Vec<Entry>,
    /// The URL the list was read from.
    pub page_url: String,
}

/// One entry of a [`Listing`].
#[derive(Debug, Clone)]
pub struct Entry {
    /// The video id the site uses, which becomes `plausible_id`'d into a URL.
    pub id: String,
    /// The entry's own URL, ready to hand back to the extractor. It is a full
    /// `watch?v=` URL rather than an id so that an unlisted Vimeo entry keeps
    /// the hash that makes it readable.
    pub url: String,
    pub title: String,
    pub duration: Option<u64>,
}

impl Listing {
    pub fn new(site: Site, id: String, page_url: String) -> Listing {
        Listing {
            site,
            id,
            title: String::new(),
            uploader: None,
            entries: Vec::new(),
            page_url,
        }
    }
}

impl Media {
    pub fn new(site: Site, id: String, page_url: String) -> Media {
        Media {
            site,
            id,
            title: String::new(),
            uploader: None,
            duration: None,
            formats: Vec::new(),
            images: Vec::new(),
            thumbnail: None,
            subtitles: Vec::new(),
            page_url,
        }
    }

    /// A stem for the output file, before `names::sanitize` has touched it.
    /// The id is appended by the caller so that two videos with the same title
    /// cannot collide silently.
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            &self.id
        } else {
            &self.title
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url::Url;

    fn site(url: &str) -> Option<Site> {
        Site::from_url(&Url::parse(url).unwrap())
    }

    #[test]
    fn recognises_the_three_sites() {
        assert_eq!(
            site("https://www.youtube.com/watch?v=x"),
            Some(Site::YouTube)
        );
        assert_eq!(site("https://youtu.be/x"), Some(Site::YouTube));
        assert_eq!(site("https://m.youtube.com/watch?v=x"), Some(Site::YouTube));
        assert_eq!(
            site("https://www.tiktok.com/@a/video/1"),
            Some(Site::TikTok)
        );
        assert_eq!(site("https://vm.tiktok.com/ZM/"), Some(Site::TikTok));
        assert_eq!(site("https://x.com/u/status/1"), Some(Site::X));
        assert_eq!(site("https://mobile.twitter.com/u/status/1"), Some(Site::X));
        assert_eq!(site("https://vimeo.com/76979871"), Some(Site::Vimeo));
        assert_eq!(
            site("https://player.vimeo.com/video/76979871"),
            Some(Site::Vimeo)
        );
        assert_eq!(site("https://vimeo.com.evil.example/1"), None);
        assert_eq!(site("https://notvimeo.com/1"), None);
        assert_eq!(
            site("https://www.reddit.com/r/aww/comments/abc/"),
            Some(Site::Reddit)
        );
        assert_eq!(site("https://v.redd.it/abc"), Some(Site::Reddit));
        assert_eq!(site("https://redd.it/abc"), Some(Site::Reddit));
        assert_eq!(site("https://notreddit.com/1"), None);
        assert_eq!(
            site("https://soundcloud.com/artist/track"),
            Some(Site::SoundCloud)
        );
        assert_eq!(
            site("https://on.soundcloud.com/abc"),
            Some(Site::SoundCloud)
        );
        assert_eq!(site("https://notsoundcloud.com/1"), None);
        assert_eq!(
            site("https://www.twitch.tv/videos/1234567890"),
            Some(Site::Twitch)
        );
        assert_eq!(site("https://m.twitch.tv/videos/42"), Some(Site::Twitch));
        assert_eq!(site("https://nottwitch.tv/videos/1"), None);
        assert_eq!(site("https://t.co/abc"), Some(Site::X));
    }

    #[test]
    fn does_not_recognise_lookalikes() {
        assert_eq!(site("https://notyoutube.com/watch?v=x"), None);
        assert_eq!(site("https://youtube.com.evil.example/watch?v=x"), None);
        assert_eq!(site("https://x.com.evil.example/a/status/1"), None);
    }

    #[test]
    fn labels_a_format_without_a_trailing_zero() {
        let mut f = Format {
            id: "137".into(),
            kind: StreamKind::Video,
            container: Container::Mp4,
            codec: "avc1".into(),
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 4_000_000,
            bytes: None,
            url: Some("https://example/1".into()),
            headers: Vec::new(),
            source: "test",
        };
        assert_eq!(f.label(), "1080p");
        f.fps = 60;
        assert_eq!(f.label(), "1080p60");
        f.height = 0;
        assert_eq!(f.label(), "audio");
    }
}
