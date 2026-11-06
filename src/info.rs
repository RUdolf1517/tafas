//! `--write-info-json`: what was chosen, beside the file that was written.
//!
//! The document is deliberately small and deliberately **without media URLs**.
//! A signed media URL is a credential that expires in hours; writing one into a
//! file that outlives it hands the next reader a link that is either useless or,
//! worse, still live. What is kept is what stays true: the page, the title, the
//! streams the page offered, which one was taken, and what landed on disk.
//!
//! It is written the same way a media file is: to a temporary beside the target
//! and renamed only once the whole document is on disk, so an interrupted run
//! leaves no truncated JSON for a script to trip over.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::model::{Format, Media, StreamKind};
use crate::select::Plan;

/// The schema this document follows. A field's *meaning* is the contract; adding
/// one is a minor version, changing one is a major.
pub const INFO_SCHEMA_VERSION: u32 = 1;

/// `<stem>.info.json`, next to the file it describes.
pub fn sidecar_path(target: &Path) -> PathBuf {
    target.with_extension("info.json")
}

/// The document for one item.
pub fn document(media: &Media, plan: &Plan, files: &[PathBuf]) -> Value {
    json!({
        "schema_version": INFO_SCHEMA_VERSION,
        "tafas": env!("CARGO_PKG_VERSION"),
        "site": media.site.as_str(),
        "id": media.id,
        "title": media.title,
        "uploader": media.uploader,
        "duration_seconds": media.duration,
        "page_url": media.page_url,
        "selected": plan.describe(),
        "formats": media.formats.iter().map(format_value).collect::<Vec<_>>(),
        "files": files
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
    })
}

fn format_value(format: &Format) -> Value {
    json!({
        "id": format.id,
        "kind": match format.kind {
            StreamKind::Muxed => "muxed",
            StreamKind::Video => "video",
            StreamKind::Audio => "audio",
        },
        "container": format.container.name(),
        "codec": format.codec,
        "width": format.width,
        "height": format.height,
        "fps": format.fps,
        "bitrate": format.bitrate,
        "bytes": format.bytes,
        "source": format.source,
    })
}

/// Writes the sidecar for `target` and returns where it went.
pub fn write(target: &Path, document: &Value) -> Result<PathBuf> {
    let path = sidecar_path(target);
    let text = serde_json::to_string_pretty(document)
        .map_err(|error| Error::Internal(format!("could not encode the info document: {error}")))?;

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
        file.write_all(text.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(Error::Io(io::Error::new(
            error.kind(),
            format!("could not write {}: {error}", path.display()),
        )));
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(Error::Io(io::Error::new(
            error.kind(),
            format!("could not write {}: {error}", path.display()),
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Container, Site};

    fn media() -> Media {
        let mut media = Media::new(
            Site::YouTube,
            "aqz-KE-bpKQ".to_string(),
            "https://www.youtube.com/watch?v=aqz-KE-bpKQ".to_string(),
        );
        media.title = "Big Buck Bunny".to_string();
        media.uploader = Some("Blender".to_string());
        media.duration = Some(596);
        media.formats.push(Format {
            id: "137".to_string(),
            kind: StreamKind::Video,
            container: Container::Mp4,
            codec: "avc1.640028".to_string(),
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 4_000_000,
            bytes: Some(123_456),
            url: Some("https://example.test/videoplayback?token=secret".to_string()),
            headers: Vec::new(),
            source: "youtube-watch",
        });
        media
    }

    fn plan() -> Plan {
        Plan::Single(media().formats[0].clone())
    }

    #[test]
    fn the_document_describes_the_media_and_the_choice() {
        let document = document(&media(), &plan(), &[PathBuf::from("Big Buck Bunny.mp4")]);
        assert_eq!(document["schema_version"], INFO_SCHEMA_VERSION);
        assert_eq!(document["site"], "youtube");
        assert_eq!(document["id"], "aqz-KE-bpKQ");
        assert_eq!(document["title"], "Big Buck Bunny");
        assert_eq!(document["uploader"], "Blender");
        assert_eq!(document["duration_seconds"], 596);
        assert_eq!(document["formats"][0]["kind"], "video");
        assert_eq!(document["formats"][0]["container"], "mp4");
        assert_eq!(document["formats"][0]["source"], "youtube-watch");
        assert_eq!(document["files"][0], "Big Buck Bunny.mp4");
        assert!(
            document["selected"].as_str().unwrap().contains("1080p"),
            "{}",
            document["selected"]
        );
    }

    #[test]
    fn no_media_url_is_written_down() {
        let document = document(&media(), &plan(), &[]);
        let text = document.to_string();
        assert!(
            !text.contains("token=secret"),
            "a signed URL expires and must not outlive the run: {text}"
        );
    }

    #[test]
    fn the_sidecar_sits_beside_its_target() {
        assert_eq!(
            sidecar_path(Path::new("/tmp/clip.mp4")),
            PathBuf::from("/tmp/clip.info.json")
        );
    }

    #[test]
    fn writing_is_atomic_and_leaves_no_temporary() {
        let directory = std::env::temp_dir().join(format!(
            "tafas-info-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let target = directory.join("clip.mp4");

        let path = write(&target, &document(&media(), &plan(), &[])).unwrap();
        assert_eq!(path, directory.join("clip.info.json"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        assert!(serde_json::from_str::<Value>(&text).is_ok());
        assert!(
            !directory.join("clip.info.json.part").exists(),
            "the temporary is gone once the rename has happened"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
