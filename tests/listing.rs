//! A page that names other videos, in the parts that do not need the internet.
//!
//! The pages themselves are read by `sites::youtube_list`,
//! `sites::vimeo_album` and `sites::twitch_collection`, whose own tests cover
//! the documents: URL shapes, entries, paging and the refusals. What is left
//! for an integration test is the arithmetic a user sees — which entries are
//! chosen, what they are called, and where they go — because that is the part
//! `ls` agrees or disagrees with, and it is the same for all three sites.

use std::path::Path;

use tafas::items::Items;
use tafas::listing;
use tafas::model::{Entry, Listing, Site};

fn listing(count: usize) -> Listing {
    let mut listing = Listing::new(
        Site::YouTube,
        "PLabc".into(),
        "https://www.youtube.com/playlist?list=PLabc".into(),
    );
    listing.title = "A playlist".into();
    listing.entries = (1..=count)
        .map(|index| Entry {
            id: format!("id{index:09}"),
            url: format!("https://www.youtube.com/watch?v=id{index}"),
            title: format!("Some / title: {index}"),
            duration: Some(30),
        })
        .collect();
    listing
}

#[test]
fn every_entry_is_chosen_without_a_selection() {
    let chosen = listing::choose(&listing(4), None).expect("all of them");
    assert_eq!(chosen.len(), 4);
    assert_eq!(chosen[0].0, 1, "positions are 1-based");
    assert_eq!(chosen[3].0, 4);
}

#[test]
fn a_selection_keeps_the_numbers_the_page_shows() {
    let items = Items::parse("2-3").expect("a list");
    let chosen = listing::choose(&listing(4), Some(&items)).expect("two entries");
    assert_eq!(
        chosen
            .iter()
            .map(|(position, _)| *position)
            .collect::<Vec<_>>(),
        vec![2, 3],
        "--playlist-items 2-3 writes 02 and 03, which is what the user counted"
    );
}

#[test]
fn an_entry_is_named_for_its_position_and_findable_by_its_id() {
    let items = Items::parse("2").expect("one entry");
    let chosen = listing::choose(&listing(3), Some(&items)).expect("one");
    assert_eq!(
        listing::stem(chosen[0].0, &chosen[0].1),
        "02 - Some _ title_ 2 [id000000002]"
    );
}

#[test]
fn a_selection_that_matches_nothing_names_the_count_it_could_have_had() {
    let items = Items::parse("9").expect("a list");
    let error = listing::choose(&listing(3), Some(&items)).expect_err("out of range");
    assert!(error.to_string().contains("selects nothing"), "{error}");
    assert!(error.to_string().contains('3'), "{error}");
}

#[test]
fn a_list_is_written_into_a_directory_and_never_into_one_file() {
    assert_eq!(
        listing::directory(None, &listing(2)).unwrap(),
        Path::new(".")
    );
    assert_eq!(
        listing::directory(Some(Path::new("videos")), &listing(2)).unwrap(),
        Path::new("videos")
    );
    let error = listing::directory(Some(Path::new("one.mp4")), &listing(2))
        .expect_err("one file cannot hold two videos");
    let text = error.to_string();
    assert!(text.contains("pass a directory"), "{text}");
    assert!(
        text.contains('2'),
        "the message says how many there are: {text}"
    );
}

#[test]
fn the_listing_line_says_which_of_how_many() {
    let listing = listing(4);
    let items = Items::parse("2-3").expect("a list");
    let chosen = listing::choose(&listing, Some(&items)).expect("two");
    let text = listing::describe(&listing, &chosen);
    assert!(text.contains("2 of 4 entries"), "{text}");
    assert!(
        text.contains("title: 2") && text.contains("title: 3"),
        "{text}"
    );
}

/// The three sites with list pages are the ones the reader table says have
/// them, and the table is what `--capabilities` prints. A site whose list
/// reader was added without a row would be a page nothing dispatches to.
#[test]
fn every_site_with_list_pages_states_them_in_capabilities() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tafas"))
        .arg("--capabilities")
        .output()
        .expect("the binary runs");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("capabilities is JSON");
    let pages: Vec<&str> = value["listings"]["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .map(|page| page.as_str().unwrap_or(""))
        .collect();
    for site in ["youtube", "vimeo", "twitch"] {
        assert!(
            pages.iter().any(|page| page.starts_with(site)),
            "--capabilities does not name a {site} list page: {pages:?}"
        );
    }
    assert_eq!(value["listings"]["depth"], 1, "one level, as documented");
}

/// A list of a site that only has videos, built the way the download layer
/// builds one, still numbers and names its entries: the rule lives in
/// `listing`, not in any one site's reader.
#[test]
fn the_naming_and_selection_rules_do_not_belong_to_one_site() {
    let mut album = Listing::new(
        Site::Vimeo,
        "2632481".into(),
        "https://vimeo.com/showcase/2632481".into(),
    );
    album.title = "Staff Favorites".into();
    album.entries = (1..=3)
        .map(|index| Entry {
            id: format!("7697987{index}"),
            url: format!("https://vimeo.com/7697987{index}"),
            title: format!("Video {index}"),
            duration: Some(60),
        })
        .collect();
    let chosen = listing::choose(&album, Some(&Items::parse("2-").expect("a list")));
    let chosen = chosen.expect("from the second to the end");
    assert_eq!(chosen.len(), 2);
    assert_eq!(
        chosen[0].0, 2,
        "the number on disk is the number in the browser"
    );
    assert_eq!(
        listing::stem(chosen[0].0, &chosen[0].1),
        "02 - Video 2 [76979872]"
    );
    let text = listing::describe(&album, &chosen);
    assert!(text.contains("vimeo 2632481"), "{text}");
    assert!(text.contains("2 of 3 entries"), "{text}");
}

/// `--name` and `--format` describe one file, and a list is several. The docs
/// say both are dropped inside a list; the code dropped `format` and left
/// `name` set, where `target_path` computed a stem from it and threw it away.
/// That worked, and it read as a bug — so the drop is asserted here, together
/// with the rule it protects: an entry is named for its position and id.
#[test]
fn a_list_drops_the_flags_that_describe_one_file() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/download.rs"),
    )
    .expect("download.rs is in the tree");

    let start = source
        .find("entry_options.multiple = true;")
        .expect("the list builds per-entry options");
    let window = &source[start..start + 900];
    for cleared in [
        "entry_options.format = None;",
        "entry_options.name = None;",
        "entry_options.playlist_items = None;",
    ] {
        assert!(
            window.contains(cleared),
            "a list entry keeps a flag that describes one file: {cleared} is missing"
        );
    }
}

/// Every writer of a path goes through `paths::verbatim`, because a sidecar sits
/// beside the media and inherits its depth: on Windows a `.info.json`, `.vtt` or
/// thumbnail at a deep path failed at 260 characters while the download it
/// described had succeeded. Checked as source, since the failure is only
/// reachable on one platform — which is exactly why it went unnoticed.
#[test]
fn every_file_written_beside_the_media_can_reach_it() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ["src/info.rs", "src/subtitles.rs", "src/thumbnails.rs"] {
        let text = std::fs::read_to_string(root.join(file)).expect("a writer is in the tree");
        let writes = text.matches("File::create(").count();
        assert!(writes > 0, "{file} writes a file");
        assert!(
            !text.contains("File::create(&temporary)"),
            "{file} creates its temporary without paths::verbatim"
        );
        assert!(
            text.contains("paths::verbatim(&temporary)"),
            "{file} must open its temporary in the verbatim form"
        );
        // rustfmt breaks the call across lines in one of the three, so the
        // rename is checked by its two arguments rather than as one line.
        let renamed = text
            .split("fs::rename(")
            .nth(1)
            .expect("a writer renames its temporary into place");
        assert!(
            renamed.contains("crate::paths::verbatim(&temporary)")
                && renamed.contains("crate::paths::verbatim(&path)"),
            "{file} must rename into place in the verbatim form"
        );
    }
}
