//! The table of readers: one row per site, and the only list of them.
//!
//! Before this module every site was a `match` arm in `sites::extract` plus a
//! name in `SITES` plus a host list in `Site::from_url`, which meant three
//! files to edit and a hand-kept promise that they agreed. They drifted, and
//! `tests/packaging.rs` grew a test to notice.
//!
//! Now there is one table. A reader is a plain [`Reader`] value — a name, the
//! hosts it answers for, and function pointers — so adding a site is adding a
//! row, and a build with a site compiled out is a row that is `#[cfg]`-ed away.
//! The dispatch, `--capabilities` and the "this build reads …" refusal are all
//! derived from the table, so they cannot disagree with it.
//!
//! No trait objects and no boxing: a `Reader` is a `'static` struct of
//! monomorphic function pointers, which is what makes "feature per site" a
//! smaller binary rather than a runtime check.

use crate::error::{Error, Result};
use crate::extractor_args::ExtractorArgs;
use crate::http::Http;
use crate::model::{Listing, Media};
use crate::url::Url;

use super::youtube_list;

/// What a URL names: one video, or a page that lists videos.
///
/// It is an enum rather than an error because "this page lists twelve videos"
/// is an answer: `--playlist-items` means the same thing for both, and a list
/// URL is the one thing a user can hand a downloader expecting it to work.
pub enum Target {
    Video(Box<Media>),
    List(Listing),
}

/// One site, as a value.
pub struct Reader {
    /// The name used everywhere a human sees it: `--capabilities`, the "this
    /// build reads …" refusal, the per-item JSON.
    pub name: &'static str,
    /// Every host this reader answers for.
    pub hosts: &'static [&'static str],
    /// Whether `--extractor-args` has anything to say to this reader.
    pub args: bool,
    /// `Some` when a URL can be a *list* page, which the reader then answers
    /// with entries instead of a stream. YouTube is the only one today.
    pub listing: Option<fn(&Url) -> Option<youtube_list::Kind>>,
    /// The video reader.
    pub video: fn(&Http, &Url, &str, &ExtractorArgs) -> Result<Media>,
}

impl Reader {
    /// Reads `raw_url`, which this reader was chosen for.
    ///
    /// The list route is asked first: a list page has no player response, so
    /// the video reader would answer "not a video URL", which is true and
    /// useless.
    pub fn read(
        &self,
        http: &Http,
        url: &Url,
        raw_url: &str,
        args: &ExtractorArgs,
    ) -> Result<Target> {
        if let Some(kind_of) = self.listing {
            if let Some(kind) = kind_of(url) {
                let listing = youtube_list::extract(http, url, &kind, raw_url)?;
                return Ok(Target::List(listing));
            }
        }
        (self.video)(http, url, raw_url, args).map(|media| Target::Video(Box::new(media)))
    }

    /// Whether this reader claims `host`, after the `m.`/`mobile.` prefixes a
    /// link from a phone carries are stripped.
    pub fn claims(&self, host: &str) -> bool {
        let host = host.strip_prefix("m.").unwrap_or(host);
        let host = host.strip_prefix("mobile.").unwrap_or(host);
        self.hosts.contains(&host)
    }
}

/// The readers in this build, in the order `--capabilities` lists them.
///
/// The `cfg` is the whole of "feature per site": a build without the chip is a
/// table without the row, and no code for that site is compiled at all.
pub const READERS: &[Reader] = &[
    #[cfg(feature = "youtube")]
    Reader {
        name: "youtube",
        hosts: &[
            "youtube.com",
            "youtu.be",
            "music.youtube.com",
            "youtube-nocookie.com",
        ],
        args: true,
        listing: Some(youtube_list::kind),
        video: super::youtube::extract,
    },
    #[cfg(feature = "tiktok")]
    Reader {
        name: "tiktok",
        hosts: &[
            "tiktok.com",
            "vt.tiktok.com",
            "vm.tiktok.com",
            "m.tiktok.com",
        ],
        args: true,
        listing: None,
        video: super::tiktok::extract,
    },
    #[cfg(feature = "x")]
    Reader {
        name: "x",
        hosts: &["x.com", "twitter.com", "t.co", "mobile.twitter.com"],
        args: false,
        listing: None,
        video: super::x::extract,
    },
    #[cfg(feature = "vimeo")]
    Reader {
        name: "vimeo",
        hosts: &["vimeo.com", "player.vimeo.com"],
        args: false,
        listing: None,
        video: super::vimeo::extract,
    },
    #[cfg(feature = "reddit")]
    Reader {
        name: "reddit",
        hosts: &["reddit.com", "old.reddit.com", "redd.it", "v.redd.it"],
        args: false,
        listing: None,
        video: super::reddit::extract,
    },
    #[cfg(feature = "soundcloud")]
    Reader {
        name: "soundcloud",
        hosts: &["soundcloud.com", "on.soundcloud.com"],
        args: false,
        listing: None,
        video: super::soundcloud::extract,
    },
    #[cfg(feature = "twitch")]
    Reader {
        name: "twitch",
        hosts: &["twitch.tv"],
        args: false,
        listing: None,
        video: super::twitch::extract,
    },
];

/// The reader for a host, if this build has one.
pub fn for_host(host: &str) -> Option<&'static Reader> {
    READERS.iter().find(|reader| reader.claims(host))
}

/// The names of the readers this build has, in table order.
pub fn names() -> Vec<&'static str> {
    READERS.iter().map(|reader| reader.name).collect()
}

/// The names as one sentence, for the `unsupported` refusal and for
/// `--capabilities`. Built from the table, so a build with three sites cannot
/// claim seven.
pub fn describe_build() -> String {
    names().join(", ")
}

/// Reads a URL with the reader that claims its host.
pub fn extract(http: &Http, raw_url: &str, args: &ExtractorArgs) -> Result<Target> {
    let url = Url::parse(raw_url)?;
    let host = url.host().unwrap_or_default();
    let reader = for_host(host).ok_or_else(|| {
        Error::Unsupported(format!(
            "no extractor for {raw_url}: this build reads {}",
            describe_build()
        ))
    })?;
    reader.read(http, &url, raw_url, args)
}
