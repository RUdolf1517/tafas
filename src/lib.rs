#![forbid(unsafe_code)]

//! `tafas` — capture a video from YouTube, TikTok, X, Vimeo, Reddit, SoundCloud
//! or Twitch into one playable file.
//!
//! The crate is a library with a thin binary on top, because the parts that
//! deserve tests — URL parsing, stream selection, naming, the MP4 remuxer — are
//! not the parts that need a terminal. `main.rs` contains no logic the library
//! does not expose, and everything below is the same code the binary runs: there
//! is no second, private path for library users.
//!
//! # Three ways to use it
//!
//! **As a program.** `cargo install tafas` puts the binary on the path; the
//! manual page and `--help` describe it. Nothing below is needed for that.
//!
//! **Ask what a URL holds, without downloading it.** This is the shortest useful
//! thing the library does:
//!
//! ```no_run
//! use tafas::http::Http;
//! use tafas::sites;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let http = Http::new(3, 60, 0)?;                    // retries, timeout, verbosity
//! let args = tafas::extractor_args::ExtractorArgs::default();
//! match sites::extract(&http, "https://vimeo.com/76979871", &args)? {
//!     sites::Target::Video(media) => {
//!         println!("{} — {}", media.title, media.site);
//!         for format in &media.formats {
//!             println!("  {} {} {:?}", format.id, format.height, format.container);
//!         }
//!     }
//!     // A playlist, a channel, an album, a set: entries, each with its own URL.
//!     sites::Target::List(listing) => {
//!         for entry in &listing.entries {
//!             println!("  {} {}", entry.id, entry.title);
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! **Do the whole capture.** [`download::run`] is what the binary calls: it
//! extracts, chooses a stream, fetches, verifies and writes, and returns the
//! process exit code. Building it means filling in [`download::Options`], which
//! is the command line in struct form — its `Default` is the same defaults the
//! binary has.
//!
//! ```no_run
//! use tafas::download::{self, Options};
//!
//! # fn main() {
//! let mut options = Options::default();
//! options.output = Some(std::path::PathBuf::from("captures"));
//! options.quality = tafas::select::Quality::Best;
//!
//! let code = download::run(&["https://vimeo.com/76979871".to_string()], &options);
//! std::process::exit(i32::from(code));
//! # }
//! ```
//!
//! # What the library promises, and what it does not
//!
//! The promises are the ones [docs/guarantees.md](https://github.com/RUdolf1517/tafas/blob/main/docs/guarantees.md)
//! makes to a user, unchanged: no re-encoding, nothing half-written surviving a
//! failure, and a refusal with a name instead of a silent skip. An error is
//! [`error::Error`] with a class — usage, http, refused, unsupported, limit,
//! verify — and [`error::Error::kind`] is the stable spelling of that class for a
//! caller that wants to branch without matching on prose.
//!
//! What is **not** promised yet: this is a pre-1.0 surface and the module layout
//! follows the program's own. Types appear and move as the sites do, and the
//! version number is the compatibility statement — `0.x` means "read the
//! changelog". If you depend on something below, the changelog entry for the
//! release that would break it says so under *Changed*.
//!
//! # Sites and features
//!
//! One feature per site, all on by default. A build without `vimeo` contains no
//! Vimeo code at all — not a row that refuses — which is the point of the
//! features being per site rather than per capability:
//!
//! ```toml
//! tafas = { version = "0.2", default-features = false, features = ["youtube"] }
//! ```
//!
//! [`sites::names`] answers what a build actually has, and it is derived from
//! the same table the extractors are dispatched from, so it cannot claim more.

pub mod cli;
pub mod cookies;
pub mod download;
pub mod error;
pub mod extractor_args;
pub mod hls;
pub mod hlsfetch;
pub mod http;
pub mod info;
pub mod items;
pub mod listing;
pub mod model;
pub mod mp4read;
pub mod mp4write;
pub mod mux;
pub mod names;
pub mod paths;
pub mod progress;
pub mod ratelimit;
pub mod report;
pub mod select;
pub mod sites;
pub mod style;
pub mod subtitles;
pub mod thumbnails;
pub mod ts;
pub mod url;
pub mod vimeo_password;

/// What every reader returns, and the site it came from — re-exported at the
/// root because they are in the first sentence of the examples above and of the
/// readme. A caller who has to write `tafas::model::Site` to ask which site a URL
/// names has found a layout decision that leaked.
pub use crate::model::{Entry, Format, Listing, Media, Site};

/// One URL read into a [`Media`], with the defaults the program uses for the
/// HTTP client and for `--extractor-args`.
///
/// This is [`download::run`] minus the writing: it answers what the page holds
/// and stops there. It is the whole of `tafas --list-formats` for one video,
/// which is why it is here rather than in an example.
///
/// A list page is **not** expanded: a playlist, a channel, an album or a set is
/// a different answer (see [`sites::Target`]), and pretending otherwise would
/// mean this function could return one video for a URL naming forty.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let media = tafas::inspect("https://vimeo.com/76979871")?;
/// println!("{} ({} formats)", media.title, media.formats.len());
/// # Ok(())
/// # }
/// ```
pub fn inspect(url: &str) -> error::Result<Media> {
    let http = http::Http::new(defaults::RETRIES, defaults::TIMEOUT, 0)?;
    let args = extractor_args::ExtractorArgs::default();
    match sites::extract(&http, url, &args)? {
        sites::Target::Video(media) => Ok(*media),
        sites::Target::List(listing) => Err(error::Error::Unsupported(format!(
            "{} {}: this URL is a list of {} entries, not one video — read it with \
             `download::run` (or `tafas <url>`), which captures each entry",
            listing.site,
            listing.id,
            listing.entries.len()
        ))),
    }
}

/// The site a URL names, if this build reads it.
///
/// It never touches the network: the answer comes from the same host table that
/// dispatches the extractors, so a URL this returns `Some` for is one
/// [`inspect`] can be asked about.
///
/// ```
/// use tafas::Site;
/// assert_eq!(tafas::site("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), Some(Site::YouTube));
/// assert_eq!(tafas::site("https://vimeo.com/76979871"), Some(Site::Vimeo));
/// assert_eq!(tafas::site("https://example.com/clip.mp4"), None);
/// ```
pub fn site(url: &str) -> Option<Site> {
    Site::from_url(&url::Url::parse(url).ok()?)
}

/// The defaults the binary ships with, so a library caller can start from the
/// same numbers instead of inventing their own.
pub mod defaults {
    /// `--retries`.
    pub const RETRIES: u32 = 3;
    /// `--timeout`, in seconds.
    pub const TIMEOUT: u64 = 60;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_names_the_site_this_build_reads_or_none() {
        // No network: the answer comes from the reader table.
        assert_eq!(
            site("https://soundcloud.com/artist/sets/a-set"),
            Some(Site::SoundCloud),
            "a set is a list, but it is still SoundCloud's"
        );
        assert_eq!(site("https://player.vimeo.com/video/1"), Some(Site::Vimeo));
        assert_eq!(site("not a url"), None);
        assert_eq!(site("https://example.com/x"), None);
        assert_eq!(
            site("https://notyoutube.com/watch?v=x"),
            None,
            "a lookalike host is not the site it imitates"
        );
    }
}
