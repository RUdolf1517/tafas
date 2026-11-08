//! Paths as the platform wants them.
//!
//! Win32 stops at 260 characters unless the path is handed to it *verbatim*: a
//! `\\?\` prefix, no forward slashes, no `.` or `..` in it. The extension is
//! Win32's own, it is what `std::fs` gives you no way to ask for, and it is the
//! difference between a deep output directory working and an `io` error.
//!
//! Everywhere else this is the identity — which is why the call sites can read
//! `paths::verbatim(path)` and mean "the file I named", not "a Windows thing".

use std::path::{Path, PathBuf};

/// The same path, in the form the filesystem will accept whatever its length.
///
/// A path is left alone when prefixing could change its meaning rather than
/// lengthen it: a relative path (the current directory is applied by the
/// filesystem, and `\\?\` forbids relative parts), a path that is already
/// verbatim, one with `.` or `..` components, and one written with forward
/// slashes. A UNC path is rewritten into the `\\?\UNC\` form.
pub fn verbatim(path: &Path) -> PathBuf {
    imp::verbatim(path)
}

#[cfg(not(windows))]
mod imp {
    use std::path::{Path, PathBuf};

    pub fn verbatim(path: &Path) -> PathBuf {
        path.to_path_buf()
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use super::usable;

    const BACKSLASH: u16 = b'\\' as u16;

    pub fn verbatim(path: &Path) -> PathBuf {
        if !usable(path) {
            return path.to_path_buf();
        }
        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        if units.len() < 2 {
            return path.to_path_buf();
        }
        let prefixed = if units[0] == BACKSLASH && units[1] == BACKSLASH {
            // `\\server\share\x` is a UNC path, and the verbatim form is
            // `\\?\UNC\server\share\x` rather than `\\?\\\server\share\x`.
            let mut out: Vec<u16> = "\\\\?\\UNC".encode_utf16().collect();
            out.extend_from_slice(&units[2..]);
            out
        } else {
            let mut out: Vec<u16> = "\\\\?\\".encode_utf16().collect();
            out.extend_from_slice(&units);
            out
        };
        PathBuf::from(OsString::from_wide(&prefixed))
    }
}

/// Whether prefixing `path` can only make it longer, never different.
#[cfg_attr(not(windows), allow(dead_code))]
fn usable(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let text = path.to_string_lossy();
    if text.starts_with("\\\\?\\") || text.starts_with("\\\\.\\") {
        return false;
    }
    // A verbatim path is taken literally: `.` and `..` mean nothing to it, so a
    // path that has them is not the path the caller meant.
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )
    }) {
        return false;
    }
    // Forward slashes are directory separators to Win32 but ordinary characters
    // to a verbatim path.
    !text.contains('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_the_path_is_the_path() {
        let path = Path::new("/tmp/tafas/clip.mp4");
        assert_eq!(verbatim(path), path);
    }

    #[cfg(windows)]
    #[test]
    fn an_absolute_path_gets_the_prefix() {
        let prefixed = verbatim(Path::new(r"C:\Users\a\very\deep\clip.mp4"));
        assert_eq!(
            prefixed,
            PathBuf::from(r"\\?\C:\Users\a\very\deep\clip.mp4")
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_unc_path_keeps_its_server_and_share() {
        let prefixed = verbatim(Path::new(r"\\server\share\clip.mp4"));
        assert_eq!(prefixed, PathBuf::from(r"\\?\UNC\server\share\clip.mp4"));
    }

    #[cfg(windows)]
    #[test]
    fn what_could_change_meaning_is_left_alone() {
        for path in [
            r"clip.mp4",
            r"deep\clip.mp4",
            r"C:\a\..\b.mp4",
            r"C:/a/b.mp4",
            r"\\?\C:\already\verbatim.mp4",
        ] {
            assert_eq!(
                verbatim(Path::new(path)),
                Path::new(path),
                "{path} is not made longer by being made verbatim"
            );
        }
    }

    /// The point of the extension: a path past the 260-character limit is
    /// written, renamed and deleted like any other.
    #[cfg(windows)]
    #[test]
    fn a_path_past_the_old_limit_is_written_and_renamed() {
        let deep = std::env::temp_dir()
            .join("a".repeat(120))
            .join("b".repeat(120));
        std::fs::create_dir_all(verbatim(&deep)).expect("a deep directory");
        let part = deep.join("clip.mp4.part");
        let target = deep.join("clip.mp4");
        std::fs::write(verbatim(&part), b"bytes").expect("a long write");
        std::fs::rename(verbatim(&part), verbatim(&target)).expect("a long rename");
        assert_eq!(std::fs::read(verbatim(&target)).unwrap(), b"bytes");
        assert!(
            target.as_os_str().len() > 260,
            "the test is about long paths"
        );
        let _ = std::fs::remove_dir_all(verbatim(&std::env::temp_dir().join("a".repeat(120))));
    }
}
