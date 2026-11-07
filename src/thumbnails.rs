//! `--write-thumbnail`: the poster frame, beside the file it belongs to.
//!
//! The image is whatever the site serves, copied byte for byte: no scaling, no
//! re-encoding, no conversion to a canonical format. That is the same rule the
//! video path follows, and it means the extension has to be honest — a `.webp`
//! renamed to `.jpg` is a file half the viewers refuse.
//!
//! The write goes through `<target>.<ext>.part` and a rename, like every other
//! file this program produces, so an interrupted run leaves nothing that looks
//! finished.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::http::Http;
use crate::model::Image;

/// A thumbnail larger than this is not a thumbnail.
pub const LIMIT: u64 = 8 * 1024 * 1024;

/// `<stem>.<ext>`, next to the media file.
pub fn path(target: &Path, thumbnail: &Image) -> PathBuf {
    let extension = extension_of(&thumbnail.url).unwrap_or(thumbnail.extension);
    target.with_extension(extension)
}

/// The extension a URL implies, when it implies one of the four this program
/// will write. `None` means "ask the site", and every site names a default.
pub fn extension_of(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or("");
    let (_, extension) = last.rsplit_once('.')?;
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("jpg"),
        "png" => Some("png"),
        "webp" => Some("webp"),
        "gif" => Some("gif"),
        _ => None,
    }
}

/// Fetches the image and writes it beside `target`, returning the path.
pub fn write(http: &Http, thumbnail: &Image, target: &Path) -> Result<PathBuf> {
    let headers: Vec<(&str, &str)> = thumbnail
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let bytes = http.get_bytes("thumbnail", &thumbnail.url, &headers, None, LIMIT)?;
    if bytes.is_empty() {
        return Err(Error::Http(format!(
            "{}: the thumbnail was empty",
            thumbnail.url
        )));
    }

    let path = path(target, thumbnail);
    let mut temporary = path.clone().into_os_string();
    temporary.push(".part");
    let temporary = PathBuf::from(temporary);

    let written = (|| -> io::Result<()> {
        if let Some(parent) = temporary.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(Error::Io(io::Error::new(
            error.kind(),
            format!("could not write {}: {error}", path.display()),
        )));
    }

    fs::rename(&temporary, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        Error::Io(io::Error::new(
            error.kind(),
            format!("could not put {} in place: {error}", path.display()),
        ))
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(url: &str) -> Image {
        Image {
            url: url.to_string(),
            extension: "jpg",
            headers: Vec::new(),
        }
    }

    #[test]
    fn a_known_extension_is_kept() {
        assert_eq!(extension_of("https://i.ytimg.com/vi/x/maxresdefault.jpg"), Some("jpg"));
        assert_eq!(extension_of("https://cdn.example/cover.jpeg"), Some("jpg"));
        assert_eq!(extension_of("https://cdn.example/cover.webp"), Some("webp"));
        assert_eq!(extension_of("https://cdn.example/cover.png"), Some("png"));
    }

    #[test]
    fn a_query_string_is_not_an_extension() {
        assert_eq!(
            extension_of("https://cdn.example/cover.jpg?sig=abc&expires=1"),
            Some("jpg")
        );
        assert_eq!(extension_of("https://cdn.example/cover?fmt=jpg"), None);
    }

    #[test]
    fn an_unknown_shape_falls_back_to_the_site() {
        assert_eq!(extension_of("https://cdn.example/image"), None);
        assert_eq!(extension_of("https://cdn.example/image.avif"), None);
    }

    #[test]
    fn the_thumbnail_replaces_the_media_extension() {
        let target = Path::new("/videos/Big Buck Bunny [aqz-KE-bpKQ].mp4");
        assert_eq!(
            path(target, &image("https://cdn.example/cover.jpg")),
            PathBuf::from("/videos/Big Buck Bunny [aqz-KE-bpKQ].jpg")
        );
        assert_eq!(
            path(target, &image("https://cdn.example/cover.webp")),
            PathBuf::from("/videos/Big Buck Bunny [aqz-KE-bpKQ].webp")
        );
    }
}
