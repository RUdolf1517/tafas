//! `--extractor-args`: the escape hatch a site change does not need a release for.
//!
//! The rule everywhere else in this program is that every decision is an
//! argument and every argument is documented. This is the one argument whose
//! vocabulary can change without the flags changing, so it is kept small and
//! closed: a fixed list of sites, a fixed list of keys per site, and a fixed
//! list of values per key. All of it is checked **before the first request**, so
//! a typo is a usage error with the accepted spellings in it, not a download
//! that quietly took the default route.
//!
//! What it is for: a site reorders the thing the extractor reads, and the
//! default route starts failing for everyone while a second route still works.
//! `--extractor-args youtube:client=ios` is then a one-line answer today rather
//! than a patch release tomorrow. What it is *not* for: new behaviour. Every
//! value here is a route the build already has; nothing can be turned on that a
//! release did not ship.

use crate::error::{Error, Result};

/// The InnerTube clients, in the order the chain tries them by default. Pin one
/// with `youtube:client=NAME`.
pub const YOUTUBE_CLIENTS: [&str; 5] = ["android_vr", "ios", "android", "tv_embedded", "web"];

/// TikTok's two routes: the page's rehydration document, and the mobile feed the
/// app itself calls. `tiktok:route=web` or `tiktok:route=app` narrows it to one.
pub const TIKTOK_ROUTES: [&str; 2] = ["web", "app"];

/// One `site:key=value` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    site: String,
    key: String,
    value: String,
}

/// Everything `--extractor-args` collected, in the order it was written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractorArgs {
    entries: Vec<Entry>,
}

impl ExtractorArgs {
    /// Parses one occurrence: `SITE:key=value[,key=value]`.
    ///
    /// A specification is checked against the tables above while it is parsed,
    /// which is what makes the refusal name the spellings that would have
    /// worked instead of "invalid value".
    pub fn parse(spec: &str) -> Result<ExtractorArgs> {
        let mut args = ExtractorArgs::default();
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(Error::Usage(
                "--extractor-args needs SITE:key=value; this build knows youtube:client and \
                 tiktok:route"
                    .to_string(),
            ));
        }
        for part in spec.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(Error::Usage(format!(
                    "--extractor-args {spec:?} has an empty entry in it"
                )));
            }
            let (site, rest) = part.split_once(':').ok_or_else(|| {
                Error::Usage(format!(
                    "--extractor-args {part:?} is not SITE:key=value; {}",
                    vocabulary()
                ))
            })?;
            let site = site.trim().to_ascii_lowercase();
            let (key, value) = rest.split_once('=').ok_or_else(|| {
                Error::Usage(format!(
                    "--extractor-args {part:?} is not SITE:key=value; {}",
                    vocabulary()
                ))
            })?;
            let key = key.trim().to_ascii_lowercase();
            let value = normalise(&site, &key, value.trim())?;
            if args.get(&site, &key).is_some() {
                return Err(Error::Usage(format!(
                    "--extractor-args sets {site}:{key} twice; the second value would silently win"
                )));
            }
            args.entries.push(Entry { site, key, value });
        }
        Ok(args)
    }

    /// Folds a second specification in, so a flag given twice (once per site)
    /// behaves like one flag given once.
    pub fn absorb(&mut self, other: ExtractorArgs) -> Result<()> {
        for entry in other.entries {
            if self.get(&entry.site, &entry.key).is_some() {
                return Err(Error::Usage(format!(
                    "--extractor-args sets {}:{} twice; the second value would silently win",
                    entry.site, entry.key
                )));
            }
            self.entries.push(entry);
        }
        Ok(())
    }

    /// The value for one site's key, if the user named it.
    pub fn get(&self, site: &str, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.site == site && entry.key == key)
            .map(|entry| entry.value.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The sentence a refusal ends with: everything this build accepts, spelled out.
fn vocabulary() -> String {
    format!(
        "this build knows youtube:client={} and tiktok:route={}",
        YOUTUBE_CLIENTS.join("|"),
        TIKTOK_ROUTES.join("|")
    )
}

/// Checks a value against its key's list, and returns the spelling the build
/// uses. `tv` is accepted for `tv_embedded` because that is what the docs and
/// the roadmap have always called it.
fn normalise(site: &str, key: &str, value: &str) -> Result<String> {
    let value = value.to_ascii_lowercase();
    match (site, key) {
        ("youtube", "client") => {
            let value = if value == "tv" {
                "tv_embedded".to_string()
            } else {
                value
            };
            if YOUTUBE_CLIENTS.contains(&value.as_str()) {
                Ok(value)
            } else {
                Err(Error::Usage(format!(
                    "--extractor-args youtube:client={value}: {}",
                    vocabulary()
                )))
            }
        }
        ("tiktok", "route") => {
            if TIKTOK_ROUTES.contains(&value.as_str()) {
                Ok(value)
            } else {
                Err(Error::Usage(format!(
                    "--extractor-args tiktok:route={value}: {}",
                    vocabulary()
                )))
            }
        }
        _ => Err(Error::Usage(format!(
            "--extractor-args {site}:{key}: {}",
            vocabulary()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_is_pinned_by_name() {
        let args = ExtractorArgs::parse("youtube:client=ios").unwrap();
        assert_eq!(args.get("youtube", "client"), Some("ios"));
        assert_eq!(args.get("tiktok", "route"), None);
    }

    #[test]
    fn tv_is_the_embedded_client() {
        let args = ExtractorArgs::parse("youtube:client=tv").unwrap();
        assert_eq!(args.get("youtube", "client"), Some("tv_embedded"));
    }

    #[test]
    fn several_keys_and_sites_in_one_flag() {
        let args = ExtractorArgs::parse(" youtube:client=web , tiktok:route=app ").unwrap();
        assert_eq!(args.get("youtube", "client"), Some("web"));
        assert_eq!(args.get("tiktok", "route"), Some("app"));
    }

    #[test]
    fn an_unknown_client_is_refused_with_the_list() {
        let error = ExtractorArgs::parse("youtube:client=flash").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("flash"), "{text}");
        assert!(
            text.contains("android_vr|ios|android|tv_embedded|web"),
            "{text}"
        );
    }

    #[test]
    fn a_site_without_keys_is_refused() {
        let error = ExtractorArgs::parse("x:thing=1").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("x:thing"), "{text}");
        assert!(text.contains("youtube:client"), "{text}");
    }

    #[test]
    fn a_missing_equals_or_colon_is_refused() {
        for spec in ["youtube", "youtube:client", "youtube:client="] {
            assert!(
                ExtractorArgs::parse(spec).is_err(),
                "{spec} should not parse"
            );
        }
    }

    #[test]
    fn the_same_key_twice_is_refused() {
        assert!(ExtractorArgs::parse("youtube:client=ios,youtube:client=web").is_err());
        let mut args = ExtractorArgs::parse("youtube:client=ios").unwrap();
        let second = ExtractorArgs::parse("youtube:client=web").unwrap();
        assert!(args.absorb(second).is_err());
    }
}
