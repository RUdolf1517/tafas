//! What a playlist or a channel page becomes before anything is downloaded.
//!
//! A list page names other videos, and the honest reading of `tafas <list url>`
//! is "capture each of these". That raises four questions the video path never
//! has to answer, and this module answers them in one place so `download.rs`
//! stays a pipeline:
//!
//! 1. **Selection.** `--playlist-items` is the same grammar as for an HLS
//!    segment list, 1-based, and a selection that matches nothing is a refusal
//!    naming how many entries there are.
//! 2. **Independence.** One dead entry must not cost the other nine. Each entry
//!    is captured on its own, its failure is reported, and the run's exit code
//!    is `1` if any of them failed — which is what a URL list already does.
//! 3. **Names.** A list of videos written into one directory needs names that
//!    do not collide: each entry is prefixed with its 1-based position, so
//!    `01 - title [id].mp4` sorts in page order.
//! 4. **One file.** `-o` naming a *file* for a list of videos is a refusal
//!    before the first request, because the second entry would have nowhere to
//!    go.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::items::Items;
use crate::model::{Entry, Listing};

/// The entries of `listing` this run will capture, in page order, with their
/// 1-based position.
///
/// The position is the position in the *page*, not in the selection: a user who
/// asks for `3-5` gets `03`, `04`, `05` on disk, so the number matches what they
/// counted in the browser.
pub fn choose(listing: &Listing, items: Option<&Items>) -> Result<Vec<(usize, Entry)>> {
    let total = listing.entries.len();
    let chosen: Vec<(usize, Entry)> = listing
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (index + 1, entry.clone()))
        .filter(|(index, _)| match items {
            None => true,
            Some(items) => items.contains(*index),
        })
        .collect();

    match items {
        Some(items) if chosen.is_empty() => Err(Error::Usage(format!(
            "--playlist-items {items} selects nothing: {} has {} {}",
            listing.id,
            total,
            if total == 1 { "entry" } else { "entries" }
        ))),
        _ => Ok(chosen),
    }
}

/// The directory a list is written into: `-o` when it names one, the current
/// directory otherwise.
///
/// `-o file.mp4` is refused rather than guessed at. There is no rule that turns
/// one filename into N filenames that a user could predict from the manual page,
/// and a run that invents one is a run that overwrites something later.
pub fn directory(output: Option<&Path>, listing: &Listing) -> Result<PathBuf> {
    match output {
        None => Ok(PathBuf::from(".")),
        Some(path) if path.is_dir() => Ok(path.to_path_buf()),
        Some(path) if looks_like_a_file(path) => Err(Error::Usage(format!(
            "{} names one file, but {} {} lists {} videos: pass a directory, and each entry is \
             written into it as \"<position> - <title> [<id>].<ext>\"",
            path.display(),
            listing.site,
            listing.id,
            listing.entries.len()
        ))),
        Some(path) => Ok(path.to_path_buf()),
    }
}

fn looks_like_a_file(path: &Path) -> bool {
    path.is_file() || (path.extension().is_some() && !path.is_dir())
}

/// The name stem for one entry: `03 - Some title [dQw4w9WgXcQ]`.
///
/// The title goes through the same reduction a video's title does — see
/// `names.rs` and `docs/safety.md` §4 — because it is the same
/// attacker-controlled string. The id is kept so the file can be found again,
/// and the position so `ls` sorts the way the page does.
pub fn stem(position: usize, entry: &Entry) -> String {
    // The title is the same attacker-controlled string a video's title is, so
    // it goes through the same reduction — and keeps room for the number and
    // the id, which is what makes the file findable.
    let title = crate::names::sanitize(&entry.title);
    let id = crate::names::sanitize(&entry.id);
    let room = crate::names::MAX_STEM_BYTES.saturating_sub(id.len() + 4);
    format!(
        "{position:02} - {} [{id}]",
        crate::names::truncate(title, room)
    )
}

/// The line a listing prints under `--list-formats`, before any bytes move.
pub fn describe(listing: &Listing, chosen: &[(usize, Entry)]) -> String {
    let mut text = format!(
        "{} {}: {} ({} of {} entries)",
        listing.site,
        listing.id,
        if listing.title.is_empty() {
            "untitled"
        } else {
            &listing.title
        },
        chosen.len(),
        listing.entries.len()
    );
    for (position, entry) in chosen {
        text.push_str(&format!(
            "\n  {position:02}  {}  [{}]",
            if entry.title.is_empty() {
                "untitled"
            } else {
                &entry.title
            },
            entry.id
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Entry, Site};

    fn listing(count: usize) -> Listing {
        let mut listing = Listing::new(Site::YouTube, "PL1".into(), "https://example/PL1".into());
        listing.title = "A playlist".into();
        listing.entries = (1..=count)
            .map(|index| Entry {
                id: format!("id{index:09}"),
                url: format!("https://example/watch?v=id{index}"),
                title: format!("Video {index}"),
                duration: Some(60),
            })
            .collect();
        listing
    }

    #[test]
    fn everything_is_chosen_without_a_selection() {
        let chosen = choose(&listing(5), None).expect("all of them");
        assert_eq!(chosen.len(), 5);
        assert_eq!(chosen[0].0, 1, "positions are 1-based");
        assert_eq!(chosen[4].0, 5);
    }

    #[test]
    fn a_selection_keeps_its_page_positions() {
        let items = Items::parse("3-4").expect("a list");
        let chosen = choose(&listing(5), Some(&items)).expect("two entries");
        assert_eq!(
            chosen
                .iter()
                .map(|(position, _)| *position)
                .collect::<Vec<_>>(),
            vec![3, 4],
            "the number on disk matches what the user counted"
        );
    }

    #[test]
    fn a_selection_that_matches_nothing_is_refused_with_the_count() {
        let items = Items::parse("9").expect("a list");
        let error = choose(&listing(5), Some(&items)).expect_err("out of range");
        let text = error.to_string();
        assert!(text.contains("selects nothing"), "{text}");
        assert!(
            text.contains('5'),
            "the message says how many there are: {text}"
        );
    }

    #[test]
    fn a_file_as_the_output_of_a_list_is_refused_before_any_request() {
        let error = directory(Some(Path::new("clip.mp4")), &listing(3))
            .expect_err("one file cannot hold three videos");
        let text = error.to_string();
        assert!(text.contains("pass a directory"), "{text}");
        assert!(text.contains('3'), "{text}");
    }

    #[test]
    fn a_directory_or_nothing_is_accepted() {
        assert_eq!(directory(None, &listing(1)).unwrap(), PathBuf::from("."));
        assert_eq!(
            directory(Some(Path::new("videos")), &listing(2)).unwrap(),
            PathBuf::from("videos"),
            "a path with no extension is a directory, created by the writer"
        );
        assert_eq!(
            directory(Some(Path::new("/tmp")), &listing(2)).unwrap(),
            PathBuf::from("/tmp"),
            "an existing directory is used as it is"
        );
    }

    #[test]
    fn a_stem_is_numbered_sanitised_and_findable_by_id() {
        let entry = Entry {
            id: "dQw4w9WgXcQ".into(),
            url: "https://example/watch?v=dQw4w9WgXcQ".into(),
            title: "A/B: \"test\"?".into(),
            duration: None,
        };
        let stem = stem(3, &entry);
        assert!(stem.starts_with("03 - "), "{stem}");
        assert!(stem.ends_with("[dQw4w9WgXcQ]"), "{stem}");
        assert!(!stem.contains('/'), "a title never adds a path: {stem}");
    }

    #[test]
    fn an_empty_title_becomes_untitled_and_is_still_numbered() {
        let entry = Entry {
            id: "abcdefghijk".into(),
            url: String::new(),
            title: "   ".into(),
            duration: None,
        };
        assert_eq!(stem(10, &entry), "10 - untitled [abcdefghijk]");
    }

    #[test]
    fn the_listing_line_names_what_is_chosen_out_of_what_exists() {
        let listing = listing(4);
        let items = Items::parse("2-3").expect("a list");
        let chosen = choose(&listing, Some(&items)).expect("two");
        let text = describe(&listing, &chosen);
        assert!(text.contains("2 of 4 entries"), "{text}");
        assert!(
            text.contains("Video 2") && text.contains("Video 3"),
            "{text}"
        );
        assert!(!text.contains("Video 1"), "{text}");
    }
}
