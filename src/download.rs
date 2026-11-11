//! The run: extract, choose, fetch, remux, commit.
//!
//! The order of operations is the safety property. Media is written to a
//! `.part` file next to the destination, verified, and only then renamed into
//! place — so an interrupted transfer, a truncated body or a failed remux can
//! never leave a file that looks finished. A rename on the same filesystem is
//! atomic, which is the whole reason the temporary lives beside the target
//! rather than in `/tmp`.
//!
//! stdout carries the written path. Everything else — progress, notes, errors,
//! the summary — goes to stderr, so `tafas URL` is usable in a pipeline and
//! `tafas URL --json` is machine-readable without a special flag for stdout.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use crate::cookies;
use crate::error::{Error, Result};
use crate::hlsfetch;
use crate::http::Http;
use crate::info;
use crate::items;
use crate::listing;
use crate::model::{Container, Format, Image, Media, Subtitle};
use crate::mp4read;
use crate::mp4write;
use crate::mux;
use crate::names;
use crate::paths;
use crate::progress::{human_bytes, Progress};
use crate::report::{self, Item, Status, Totals};
use crate::select::{self, Plan};
use crate::sites;
use crate::style::{self, Pen};
use crate::subtitles;
use crate::thumbnails;
use crate::url::Url;

/// Everything the command line decided.
#[derive(Debug, Clone)]
pub struct Options {
    pub output: Option<PathBuf>,
    /// More than one URL was given, so a bare `-o` path is a directory.
    pub multiple: bool,
    pub quality: select::Quality,
    pub audio_only: bool,
    /// `--write-info-json`: a sidecar describing the page and the choice.
    pub write_info_json: bool,
    /// `--write-thumbnail`: the poster frame, beside the file.
    pub write_thumbnail: bool,
    /// `--write-subs`: the caption tracks, beside the file.
    pub write_subs: bool,
    /// `--embed-subs`: put the cues inside the MP4 instead of beside it.
    pub embed_subs: bool,
    /// `--sub-langs`: which of them. `None` means "every track the page lists".
    pub sub_langs: Option<subtitles::Selection>,
    pub format: Option<String>,
    pub no_mux: bool,
    pub overwrite: bool,
    pub resume: bool,
    pub name: Option<String>,
    pub dry_run: bool,
    pub list_formats: bool,
    pub json: bool,
    pub quiet: bool,
    pub no_progress: bool,
    pub jobs: usize,
    pub retries: u32,
    pub timeout: u64,
    /// `--limit-rate`: the average a transfer may not exceed, in bytes per
    /// second. `None` means no limit at all.
    pub limit_rate: Option<u64>,
    /// A Netscape cookie file to send from, when the user named one.
    pub cookies: Option<PathBuf>,
    /// `--playlist-items`: which segments of an HLS list to take.
    pub playlist_items: Option<items::Items>,
    /// `--concurrent-fragments`: how many segments of one list may be in flight.
    pub concurrent_fragments: usize,
    /// `--extractor-args`: a site's route, narrowed by hand.
    pub extractor_args: crate::extractor_args::ExtractorArgs,
    pub verbose: u8,
    pub color: style::Choice,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            output: None,
            multiple: false,
            quality: select::Quality::Best,
            audio_only: false,
            write_info_json: false,
            write_thumbnail: false,
            write_subs: false,
            embed_subs: false,
            sub_langs: None,
            format: None,
            no_mux: false,
            overwrite: false,
            resume: true,
            name: None,
            dry_run: false,
            list_formats: false,
            json: false,
            quiet: false,
            no_progress: false,
            jobs: 1,
            retries: 3,
            timeout: 60,
            limit_rate: None,
            cookies: None,
            playlist_items: None,
            concurrent_fragments: 1,
            extractor_args: crate::extractor_args::ExtractorArgs::default(),
            verbose: 0,
            color: style::Choice::Auto,
        }
    }
}

/// Serialises stderr so that two worker threads cannot interleave half a line,
/// and holds the decisions about silence.
pub struct Printer {
    quiet: bool,
    json: bool,
    progress: bool,
    pen: Pen,
    lock: Mutex<()>,
}

impl Printer {
    pub fn new(options: &Options) -> Printer {
        Printer {
            quiet: options.quiet,
            json: options.json,
            // A bar per worker would overwrite itself; with several jobs the
            // per-item line is the progress report.
            progress: !options.no_progress && !options.quiet && !options.json && options.jobs <= 1,
            pen: Pen::for_stderr(options.color),
            lock: Mutex::new(()),
        }
    }

    /// Informational text: suppressed by `--quiet`.
    pub fn note(&self, text: &str) {
        if self.quiet {
            return;
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        eprintln!("{text}");
    }

    pub fn warn(&self, text: &str) {
        if self.quiet {
            return;
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        eprintln!("{}", self.pen.paint(style::YELLOW, text));
    }

    /// Errors are never suppressed: `--quiet` means "no chatter", not "no
    /// diagnostics".
    pub fn error(&self, text: &str) {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        eprintln!("{}", self.pen.paint(style::RED, text));
    }

    pub fn progress(&self, label: impl Into<String>, total: Option<u64>) -> Progress {
        Progress::new(label, total, self.progress)
    }

    pub fn wants_progress(&self) -> bool {
        self.progress
    }

    pub fn json(&self) -> bool {
        self.json
    }
}

/// Runs every URL and returns the process exit code.
pub fn run(urls: &[String], options: &Options) -> u8 {
    let started = Instant::now();
    let http = match Http::new(options.retries, options.timeout, options.verbose) {
        Ok(http) => http,
        Err(error) => {
            eprintln!("tafas: {error}");
            return error.exit_code();
        }
    };
    let http = match &options.cookies {
        None => http,
        Some(path) => match cookies::Jar::load(path) {
            Ok(jar) => {
                if options.verbose > 0 {
                    eprintln!("tafas: {} cookies from {}", jar.len(), path.display());
                }
                http.with_cookies(jar)
            }
            Err(error) => {
                eprintln!("tafas: {error}");
                return error.exit_code();
            }
        },
    };
    let http = http.with_limit_rate(options.limit_rate);
    let printer = Printer::new(options);

    // With one worker the item is announced as soon as it is done, which is what
    // makes a long run readable. With several, the workers would interleave on
    // stdout, so nothing is printed inside them and the results are announced
    // here, in the order the user typed the URLs.
    // `--list-formats` answers with a table on stdout, and two tables written by
    // two workers are one unusable table; it is also a read-only mode with
    // nothing to gain from concurrency.
    let serial = options.jobs <= 1 || urls.len() <= 1 || options.list_formats;
    let mut items: Vec<Item> = if serial {
        urls.iter()
            .flat_map(|url| process(&http, url, options, &printer, true))
            .collect()
    } else {
        let items = process_parallel(&http, urls, options, &printer);
        for item in &items {
            announce(item, options);
        }
        items
    };

    items.retain(|item| item.status != Status::Failed || !item.message.is_empty());

    let mut totals = Totals::default();
    for item in &items {
        totals.add(item);
    }
    if let Some(summary) = totals.summary(started.elapsed(), options.json) {
        printer.note(&summary);
    }

    report::exit_code(&items)
}

fn process_parallel(
    http: &Http,
    urls: &[String],
    options: &Options,
    printer: &Printer,
) -> Vec<Item> {
    let results: Mutex<Vec<((usize, usize), Item)>> = Mutex::new(Vec::with_capacity(urls.len()));
    let next = Mutex::new(0usize);
    let workers = options.jobs.min(urls.len()).max(1);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = {
                    let mut guard = next.lock().unwrap_or_else(|e| e.into_inner());
                    if *guard >= urls.len() {
                        break;
                    }
                    let index = *guard;
                    *guard += 1;
                    index
                };
                // A list expands into several items; they keep the position of
                // the URL they came from, which is the order the user typed.
                for (offset, item) in process(http, &urls[index], options, printer, false)
                    .into_iter()
                    .enumerate()
                {
                    let mut guard = results.lock().unwrap_or_else(|e| e.into_inner());
                    guard.push(((index, offset), item));
                }
            });
        }
    });

    let mut collected = results.into_inner().unwrap_or_else(|e| e.into_inner());
    collected.sort_by_key(|(key, _)| *key);
    collected.into_iter().map(|(_, item)| item).collect()
}

/// One URL, which may turn out to be a list of them.
///
/// A playlist or a channel is expanded here rather than inside `handle`: its
/// entries are separate items with their own line on stdout, their own failure
/// and their own report line, because that is what "one dead video must not
/// stop the other nine" means in the output and not only in the exit code.
fn process(
    http: &Http,
    url: &str,
    options: &Options,
    printer: &Printer,
    announce_now: bool,
) -> Vec<Item> {
    let started = Instant::now();
    let mut item = Item::new(url);

    match sites::extract(http, url, &options.extractor_args) {
        Ok(sites::Target::List(listing)) => {
            // Said here, once, before any entry is announced: a listing that
            // holds fewer entries than the page promised is a capture, not a
            // failure — and “12 of 12” printed over a set of forty is the quiet
            // lie the reader went to the trouble of counting against.
            if let Some(total) = listing.incomplete {
                let read = listing.entries.len();
                printer.warn(&format!(
                    "tafas: {} {}: the page says it holds {total} {} and this build read {read}; \
                     the {read} {} read {} captured, and nothing here claims the list is whole",
                    listing.site,
                    listing.id,
                    if total == 1 { "entry" } else { "entries" },
                    if read == 1 { "entry" } else { "entries" },
                    if read == 1 { "is" } else { "are" }
                ));
            }
            return process_list(http, listing, options, printer, announce_now);
        }
        Ok(sites::Target::Video(media)) => {
            match handle(http, *media, options, printer, &mut item) {
                Ok(status) => item.status = status,
                Err(error) => fail(&mut item, &error, printer),
            }
        }
        Err(error) => fail(&mut item, &error, printer),
    }

    item.elapsed_ms = started.elapsed().as_millis() as u64;
    if announce_now {
        announce(&item, options);
    }
    vec![item]
}

/// A playlist or a channel: every chosen entry, captured on its own.
///
/// The entries run one at a time even when `-j` is more than one. A list is
/// already several downloads, and opening `-j` segment streams inside each of
/// them is how a shared line stops being usable; a user who wants both runs the
/// entries as separate URLs.
fn process_list(
    http: &Http,
    listing: crate::model::Listing,
    options: &Options,
    printer: &Printer,
    announce_now: bool,
) -> Vec<Item> {
    // The item is returned rather than announced here: whoever is in charge of
    // stdout — `process` in serial mode, `run` in parallel mode — announces it
    // once, like any other item. Announcing it here as well is exactly how one
    // failure became two JSON objects.
    let fail_page = |url: &str, error: &Error| {
        let mut item = Item::new(url);
        item.site = listing.site.as_str().to_string();
        fail(&mut item, error, printer);
        item.elapsed_ms = 0;
        item
    };

    let chosen = match listing::choose(&listing, options.playlist_items.as_ref()) {
        Ok(chosen) => chosen,
        Err(error) => return vec![fail_page(&listing.page_url, &error)],
    };
    let directory = match listing::directory(options.output.as_deref(), &listing) {
        Ok(directory) => directory,
        Err(error) => return vec![fail_page(&listing.page_url, &error)],
    };

    if options.list_formats {
        let mut item = Item::new(&listing.page_url);
        item.site = listing.site.as_str().to_string();
        item.id = listing.id.clone();
        item.title = listing.title.clone();
        item.status = Status::Listed;
        // The description is the answer for this URL, so it is the result and
        // goes to stdout — the same stream `print_formats` uses. It used to be
        // followed by a JSON object when `--json` was also given, which put a
        // table and an object on one stream; the table is the answer, and
        // `announce` knows not to add the object to it.
        if !write_stdout(&format!("{}\n", listing::describe(&listing, &chosen))) {
            return vec![item];
        }
        return vec![item];
    }

    if !options.quiet && !options.json {
        printer.note(&format!(
            "{} {}: {} of {} entries",
            listing.site,
            listing.id,
            chosen.len(),
            listing.entries.len()
        ));
    }

    let mut items = Vec::with_capacity(chosen.len());
    for (position, entry) in chosen {
        let started = Instant::now();
        let mut item = Item::new(&entry.url);
        item.site = listing.site.as_str().to_string();
        item.id = entry.id.clone();
        item.title = entry.title.clone();
        item.duration_ms = entry.duration.map(|seconds| seconds * 1000);

        // Each entry is its own download, into the list's directory, under the
        // name its position earns it. `--name` and `--format` are the user's
        // answers about *one* file, so they do not survive into a list.
        let mut entry_options = options.clone();
        entry_options.output = Some(directory.clone());
        entry_options.name = Some(listing::stem(position, &entry));
        entry_options.multiple = true;
        entry_options.playlist_items = None;
        // `--format` and `--name` describe *one* file; a list renames each entry
        // after its position and id. Dropping `format` but not `name` left the
        // stem computed from `--name` and then thrown away by `target_path`,
        // which worked and read as a bug — `docs/cli.md` §6 says both are
        // dropped, so both are dropped here, where it can be seen.
        entry_options.format = None;
        entry_options.name = None;

        match load(http, &entry.url, &entry_options, printer, &mut item) {
            Ok(status) => item.status = status,
            Err(error) => fail(&mut item, &error, printer),
        }
        item.elapsed_ms = started.elapsed().as_millis() as u64;
        if announce_now {
            announce(&item, options);
        }
        items.push(item);
    }
    items
}

/// Records a failure on an item and says it once, on stderr.
///
/// The failure is *not* written to stdout here. Every item — failed or not — is
/// announced by the caller that owns it, once, and `--json` means one object per
/// item (`docs/cli.md` §7). Printing here as well put two objects on stdout for
/// one broken URL, which a reader piping into `jq` sees as a duplicate and a
/// script counting lines sees as two failures.
fn fail(item: &mut Item, error: &Error, printer: &Printer) {
    item.status = Status::Failed;
    item.reason = Some(error.kind().to_string());
    item.message = error.to_string();
    printer.error(&format!("tafas: {}", error));
}

/// One entry's video, read and written. A list inside a list is a refusal: a
/// channel of playlists is not something `--playlist-items` can address.
fn load(
    http: &Http,
    url: &str,
    options: &Options,
    printer: &Printer,
    item: &mut Item,
) -> Result<Status> {
    match sites::extract(http, url, &options.extractor_args)? {
        sites::Target::Video(media) => handle(http, *media, options, printer, item),
        sites::Target::List(listing) => Err(Error::Refused(format!(
            "{url} is a {} of {} videos nested inside another list, and this build follows one \
             level",
            listing.site,
            listing.entries.len()
        ))),
    }
}

/// Writes what this item produced: the path on stdout (or the JSON object),
/// and one human line on stderr.
fn announce(item: &Item, options: &Options) {
    // `--json` and `--list-formats` both answer on stdout, and the answer is one
    // object per item: the stream table is `print_formats`, which writes it, and
    // this must not put the object in the middle of it.
    if options.list_formats {
        return;
    }
    if options.json {
        println!("{}", item.to_json());
        return;
    }
    if let Some(path) = &item.path {
        // Data goes to stdout, even with `--quiet`: it is the result, not
        // chatter. A closed pipe is answered with silence, as any filter does.
        if !write_stdout(&format!("{}\n", path.display())) {
            return;
        }
    }
    if !options.quiet {
        match item.status {
            Status::Downloaded => eprintln!("{}", item.to_line()),
            Status::Planned => eprintln!("plan: {}", item.selection),
            Status::Listed => {}
            Status::Failed => {}
        }
    }
}

/// Returns false when stdout is a closed pipe.
pub fn write_stdout(text: &str) -> bool {
    match io::stdout()
        .write_all(text.as_bytes())
        .and_then(|()| io::stdout().flush())
    {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => false,
        Err(_) => false,
    }
}

fn handle(
    http: &Http,
    media: Media,
    options: &Options,
    printer: &Printer,
    item: &mut Item,
) -> Result<Status> {
    item.site = media.site.as_str().to_string();
    item.id = media.id.clone();
    item.title = media.title.clone();
    item.duration_ms = media.duration.map(|seconds| seconds * 1000);

    if options.list_formats {
        print_formats(&media, printer)?;
        item.status = Status::Listed;
        return Ok(Status::Listed);
    }

    let plan = select::choose(
        &media,
        &select::Options {
            quality: options.quality,
            audio_only: options.audio_only,
            format: options.format.clone(),
            no_mux: options.no_mux,
        },
    )?;
    item.selection = plan.describe();

    let hls_plan = matches!(&plan, Plan::Single(format) if format.container == Container::Hls);
    if options.playlist_items.is_some() && !hls_plan {
        printer.warn(
            "note: --playlist-items applies to an HLS segment list; this selection is not \
             one, so it was ignored",
        );
    }
    if options.concurrent_fragments > 1 && !hls_plan {
        printer.warn(
            "note: --concurrent-fragments applies to the segments of an HLS list; this \
             selection is not one, so it was ignored",
        );
    }

    let target = target_path(options, &media, &plan)?;

    if options.dry_run {
        item.path = Some(target.clone());
        if let Some(estimate) = plan.estimate_bytes(media.duration.unwrap_or(0)) {
            item.bytes = estimate;
        }
        if !options.json && !options.quiet {
            eprintln!(
                "{} {}: {} ({})",
                media.site,
                media.id,
                plan.describe(),
                match plan.estimate_bytes(media.duration.unwrap_or(0)) {
                    Some(bytes) => human_bytes(bytes),
                    None => "size unknown".to_string(),
                }
            );
        }
        // No JSON here: `announce` writes the object for this item when the
        // caller that owns it is done, and one item is one object.
        return Ok(Status::Planned);
    }

    if is_stdout(&target) {
        return stream_to_stdout(http, &plan, options, printer, item);
    }

    if target.exists() && !options.overwrite {
        return Err(Error::Refused(format!(
            "{} already exists; pass --overwrite to replace it",
            target.display()
        )));
    }

    let bytes = fetch_plan(http, &plan, &target, options, printer)?;
    item.bytes = bytes;

    if options.write_thumbnail {
        write_thumbnail(http, &media, &plan, &target, options, printer)?;
    }

    if options.write_subs {
        write_subtitles(http, &media, &target, options, printer)?;
    }

    if options.embed_subs {
        item.bytes = embed_subtitles(http, &media, &plan, &target, options, printer)?;
    }

    if options.write_info_json {
        if matches!(plan, Plan::Images(_)) {
            printer.warn(
                "note: --write-info-json is not written for photo posts; every image is its own \
                 file and none of them is \"the\" one",
            );
        } else {
            let mut files = vec![target.clone()];
            if let Plan::Separate {
                audio: Some(audio), ..
            } = &plan
            {
                files.push(audio_path(&target, audio));
            }
            let document = info::document(&media, &plan, &files);
            let sidecar = info::write(&target, &document)?;
            if !options.json && !options.quiet {
                printer.note(&format!("wrote {}", sidecar.display()));
            }
        }
    }

    item.path = Some(target);
    Ok(Status::Downloaded)
}

/// `--write-subs`: every caption track the selection names, as WebVTT.
///
/// Two failures are told apart on purpose. A page that lists no captions at all
/// is a note — the video is what the user asked for and it is on disk. A
/// `--sub-langs` that matches nothing is a refusal: a specific thing was asked
/// for and the page does not have it, and the message says what it does have.
fn write_subtitles(
    http: &Http,
    media: &Media,
    target: &Path,
    options: &Options,
    printer: &Printer,
) -> Result<()> {
    let selection = options
        .sub_langs
        .clone()
        .unwrap_or_else(subtitles::Selection::all);
    let chosen: Vec<&Subtitle> = media
        .subtitles
        .iter()
        .filter(|subtitle| selection.matches(&subtitle.language))
        .collect();

    if chosen.is_empty() {
        if options.sub_langs.is_some() {
            let available = if media.subtitles.is_empty() {
                "none".to_string()
            } else {
                media
                    .subtitles
                    .iter()
                    .map(|subtitle| subtitle.language.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return Err(Error::Refused(format!(
                "{} {}: no caption track matches --sub-langs {selection}; the page offers \
                 {available}",
                media.site, media.id
            )));
        }
        printer.warn("note: this page offers no caption tracks; nothing was written");
        return Ok(());
    }

    for subtitle in chosen {
        let path = subtitles::write(http, subtitle, target)?;
        if !options.json && !options.quiet {
            printer.note(&format!("wrote {}", path.display()));
        }
    }
    Ok(())
}

/// `--embed-subs`: the selected caption track, written into the MP4 this run
/// just produced.
///
/// The file is read back with `mp4read`, the cues become one more track, and the
/// whole thing is laid out again by the writer that produced it. Samples are the
/// same bytes, so nothing is re-encoded and the picture is untouched; what
/// changes is one `trak`, which is why the rewrite is a `moov` rebuild and not a
/// remux of two sources.
///
/// A run that did not produce exactly one MP4 says so and changes nothing: two
/// files, a photo post or an audio-only file is not a place a subtitle track can
/// go without inventing a second output the user did not ask for.
fn embed_subtitles(
    http: &Http,
    media: &Media,
    plan: &Plan,
    target: &Path,
    options: &Options,
    printer: &Printer,
) -> Result<u64> {
    let size = std::fs::metadata(target)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let one_mp4 = match plan {
        Plan::Single(format) => format.container == Container::Mp4,
        Plan::Pair { .. } => true,
        Plan::Separate { .. } | Plan::Audio(_) | Plan::Images(_) => false,
    };
    if !one_mp4 {
        printer.warn(
            "note: --embed-subs needs one MP4 from this run; this selection is not one, so no \
             track was added",
        );
        return Ok(size);
    }

    let selection = options
        .sub_langs
        .clone()
        .unwrap_or_else(subtitles::Selection::all);
    let chosen: Vec<&Subtitle> = media
        .subtitles
        .iter()
        .filter(|subtitle| selection.matches(&subtitle.language))
        .collect();
    if chosen.is_empty() {
        if options.sub_langs.is_some() {
            let available = if media.subtitles.is_empty() {
                "none".to_string()
            } else {
                media
                    .subtitles
                    .iter()
                    .map(|subtitle| subtitle.language.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return Err(Error::Refused(format!(
                "{} {}: no caption track matches --sub-langs {selection}; the page offers \
                 {available}",
                media.site, media.id
            )));
        }
        printer.note("note: this page offers no caption tracks; no track was added");
        return Ok(size);
    }
    if chosen.len() > 1 {
        let languages = chosen
            .iter()
            .map(|subtitle| subtitle.language.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::Usage(format!(
            "--embed-subs writes one track and --sub-langs {selection} matches {}: {languages}; \
             name one language, or use --write-subs for all of them",
            chosen.len()
        )));
    }
    let subtitle = chosen[0];
    let body = subtitles::body(http, subtitle)?;
    let cues = subtitles::cues(&body)?;
    if cues.is_empty() {
        return Err(Error::Refused(format!(
            "{}: the caption body parsed to no cues, so there is nothing to put in a track",
            subtitle.url
        )));
    }

    let bytes = std::fs::read(target).map_err(|error| {
        Error::Io(io::Error::new(
            error.kind(),
            format!(
                "could not read {} back to add a subtitle track: {error}",
                target.display()
            ),
        ))
    })?;
    let dimensions = mp4read::dimensions(&bytes).unwrap_or((0, 0));
    let mut tracks = mp4read::plain(&bytes)?;
    tracks.push(subtitles::track(&cues));

    let temporary = part_path(target);
    let report = mp4write::write(&tracks, dimensions, &temporary)?;
    std::fs::rename(paths::verbatim(&temporary), paths::verbatim(target)).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        Error::Io(io::Error::new(
            error.kind(),
            format!("could not put {} in place: {error}", target.display()),
        ))
    })?;
    if !options.json && !options.quiet {
        printer.note(&format!(
            "embedded {} ({} cues) into {}",
            subtitle.language,
            cues.len(),
            target.display()
        ));
    }
    Ok(report.bytes)
}

/// `--write-thumbnail`: the poster frame, beside the media file.
///
/// It is written after the media file has landed, and a failure here is a
/// failure of the item: the file the user asked for is on disk either way, but
/// a silent thumbnail is the kind of quiet skips this program does not do.
fn write_thumbnail(
    http: &Http,
    media: &Media,
    plan: &Plan,
    target: &Path,
    options: &Options,
    printer: &Printer,
) -> Result<()> {
    if matches!(plan, Plan::Images(_)) {
        printer.warn(
            "note: --write-thumbnail is not written for photo posts; every image is already a \
             file of its own",
        );
        return Ok(());
    }
    let Some(thumbnail) = media.thumbnail.as_ref() else {
        printer.warn("note: this page offered no thumbnail; nothing was written");
        return Ok(());
    };
    let path = thumbnails::write(http, thumbnail, target)?;
    if !options.json && !options.quiet {
        printer.note(&format!("wrote {}", path.display()));
    }
    Ok(())
}

/// `-o -`: the media bytes themselves on stdout. Only possible when the result
/// is a single stream — two streams have to be joined, and joining needs files.
fn stream_to_stdout(
    http: &Http,
    plan: &Plan,
    options: &Options,
    printer: &Printer,
    item: &mut Item,
) -> Result<Status> {
    let format =
        match plan {
            Plan::Single(format) if format.container == Container::Hls => {
                return Err(Error::Usage(format!(
                    "-o - writes one stream, and an HLS playlist ({}) is assembled from its \
                     segments into an MP4 first; give a file name instead",
                    format.label()
                )))
            }
            Plan::Single(format) | Plan::Audio(format) => Format::clone(format),
            _ => return Err(Error::Usage(
                "-o - writes one stream: this selection needs two streams or a remux, so give a \
                 file name instead"
                    .into(),
            )),
        };
    if options.write_info_json {
        printer.warn(
            "note: --write-info-json names a file and -o - is a pipe; the sidecar is skipped",
        );
    }
    let url = format
        .url
        .clone()
        .ok_or_else(|| Error::Internal("the chosen format has no URL".into()))?;
    let headers = request_headers(&format);
    let borrowed: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();

    if options.write_subs || options.write_thumbnail {
        printer.warn(
            "note: -o - writes one stream to a pipe, which has nowhere to put a sidecar; no \
             caption or thumbnail file was written",
        );
    }

    let mut progress = printer.progress(format.label(), format.bytes);
    let stdout = io::stdout();
    let mut sink = stdout.lock();
    let result = http.download(
        "media",
        &url,
        &borrowed,
        &mut sink,
        // A pipe cannot be resumed, so a partial transfer is simply gone.
        0,
        &mut |bytes| progress.add(bytes),
    );
    progress.clear();
    let report = result?;
    sink.flush()?;
    let _ = options;
    item.bytes = report.written;
    Ok(Status::Downloaded)
}

/// A path derived from the media, the plan and `-o`.
fn target_path(options: &Options, media: &Media, plan: &Plan) -> Result<PathBuf> {
    if let Some(output) = &options.output {
        if is_stdout(output) {
            return Ok(output.clone());
        }
    }

    // The stem is only meaningful where a file is named after the media: no `-o`
    // at all, or `-o` naming a directory. When `-o` names the file, the name is
    // the user's. Computed inside those branches rather than above them, so it
    // cannot be discarded silently — which is what it was doing for every list
    // entry, whose stem `process_list` had already decided.
    match &options.output {
        None => Ok(PathBuf::from(format!(
            "{}.{}",
            names::stem(media, options.name.as_deref()),
            plan.extension()
        ))),
        Some(output) => {
            let looks_like_dir = options.multiple
                || output.is_dir()
                || output.to_string_lossy().ends_with('/')
                || output.to_string_lossy().ends_with('\\');
            if looks_like_dir {
                fs::create_dir_all(paths::verbatim(output)).map_err(|e| {
                    Error::Io(io::Error::new(
                        e.kind(),
                        format!("could not create {}: {e}", output.display()),
                    ))
                })?;
                Ok(output.join(format!(
                    "{}.{}",
                    names::stem(media, options.name.as_deref()),
                    plan.extension()
                )))
            } else {
                Ok(output.clone())
            }
        }
    }
}

fn fetch_plan(
    http: &Http,
    plan: &Plan,
    target: &Path,
    options: &Options,
    printer: &Printer,
) -> Result<u64> {
    match plan {
        Plan::Single(format) if format.container == Container::Hls => {
            fetch_hls(http, format, target, options, printer)
        }
        Plan::Single(format) => {
            let bytes = fetch_stream(http, format, target, options, printer)?;
            commit(&part_path(target), target, options.overwrite)?;
            Ok(bytes)
        }
        Plan::Audio(format) => {
            let bytes = fetch_stream(http, format, target, options, printer)?;
            commit(&part_path(target), target, options.overwrite)?;
            Ok(bytes)
        }
        Plan::Images(images) => download_images(http, images, target, options, printer),
        Plan::Pair { video, audio } => {
            let video_part = suffix_path(target, ".video.part");
            let audio_part = suffix_path(target, ".audio.part");
            let muxed_part = part_path(target);

            let video_bytes = fetch_stream_to(http, video, &video_part, options, printer)?;
            let audio_bytes = match fetch_stream_to(http, audio, &audio_part, options, printer) {
                Ok(bytes) => bytes,
                Err(error) => {
                    let _ = fs::remove_file(&video_part);
                    return Err(error);
                }
            };

            printer.note(&format!(
                "muxing {} + {} (no re-encoding)",
                human_bytes(video_bytes),
                human_bytes(audio_bytes)
            ));
            let mux_result = mux::remux(&video_part, &audio_part, &muxed_part).and_then(|report| {
                let verified = mux::verify(&muxed_part)?;
                if !verified.has_handler("vide") || !verified.has_handler("soun") {
                    return Err(Error::Verify(format!(
                        "{}: the remuxed file is missing a track (found {})",
                        muxed_part.display(),
                        verified
                            .tracks
                            .iter()
                            .map(|track| track.handler.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
                Ok(report)
            });

            let report = match mux_result {
                Ok(report) => report,
                Err(error) => {
                    let _ = fs::remove_file(&video_part);
                    let _ = fs::remove_file(&audio_part);
                    let _ = fs::remove_file(&muxed_part);
                    return Err(error);
                }
            };

            commit(&muxed_part, target, options.overwrite)?;
            let _ = fs::remove_file(&video_part);
            let _ = fs::remove_file(&audio_part);
            let _ = report.duration_ms;
            Ok(report.bytes)
        }
        Plan::Separate { video, audio, why } => {
            if let Some(why) = why {
                printer.warn(&format!("note: {why}"));
            }
            let bytes = fetch_stream(http, video, target, options, printer)?;
            commit(&part_path(target), target, options.overwrite)?;

            let mut total = bytes;
            if let Some(audio) = audio {
                let audio_target = audio_path(target, audio);
                let audio_bytes = fetch_stream(http, audio, &audio_target, options, printer)?;
                commit(&part_path(&audio_target), &audio_target, options.overwrite)?;
                total += audio_bytes;
                announce_path(&audio_target, options, printer);
            }
            Ok(total)
        }
    }
}

/// Fetches an HLS stream: playlist, segments, demux, MP4.
///
/// The staging file is `<target>.part` like every other fetch, so an
/// interrupted HLS download leaves the same debris. What is resumed is not the
/// MP4 — a half-written one has no offset worth continuing from — but the
/// segment stream beside it: `<target>.part.segments` with a journal of lengths,
/// both deleted the moment the MP4 is committed. `--no-resume` removes them
/// before the first request, so a cache is never spliced into a run that did not
/// ask for one.
fn fetch_hls(
    http: &Http,
    format: &Format,
    target: &Path,
    options: &Options,
    printer: &Printer,
) -> Result<u64> {
    let url = format
        .url
        .clone()
        .ok_or_else(|| Error::Internal("the chosen format has no URL".into()))?;
    let url = Url::parse(&url)?;
    let part = part_path(target);

    if let Some(parent) = part.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(paths::verbatim(parent))?;
        }
    }

    if !options.resume {
        hlsfetch::forget(&part);
    }

    let mut progress = printer.progress(format!("{} hls", format.label()), None);
    let result = hlsfetch::fetch(
        http,
        &url,
        &request_headers(format),
        options.quality,
        hlsfetch::Segments {
            items: options.playlist_items.as_ref(),
            concurrent: options.concurrent_fragments,
            resume: options.resume.then_some(part.as_path()),
        },
        &part,
        &mut |bytes| progress.add(bytes),
    );
    progress.clear();

    let report = match result {
        Ok(report) => report,
        Err(error) => {
            let _ = fs::remove_file(&part);
            return Err(error);
        }
    };

    let verified = mux::verify(&part)?;
    if verified.sample_total() == 0 {
        let _ = fs::remove_file(&part);
        return Err(Error::Verify(format!(
            "{}: the assembled file has no samples",
            part.display()
        )));
    }

    commit(&part, target, options.overwrite)?;
    // The MP4 is the file that was asked for; the cache beside it is debris now.
    hlsfetch::forget(&part);
    for warning in &report.warnings {
        printer.warn(&format!("warning: {warning}"));
    }
    if report.resumed_segments > 0 && !options.json {
        printer.note(&format!(
            "hls: {} of {} segments came from the previous run",
            report.resumed_segments, report.segments
        ));
    }
    if !options.json {
        printer.note(&format!(
            "hls: {} segments, {}, {} video and {} audio samples",
            report.segments,
            human_bytes(report.bytes),
            report.video_samples,
            report.audio_samples
        ));
    }
    Ok(report.bytes)
}

fn announce_path(path: &Path, options: &Options, printer: &Printer) {
    if options.json {
        return;
    }
    write_stdout(&format!("{}\n", path.display()));
    if !options.quiet {
        printer.note(&format!("wrote {}", path.display()));
    }
}

fn download_images(
    http: &Http,
    images: &[Image],
    target: &Path,
    options: &Options,
    printer: &Printer,
) -> Result<u64> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let stem = target
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());

    let mut total = 0u64;
    for (index, image) in images.iter().enumerate() {
        let path = if images.len() == 1 {
            target.to_path_buf()
        } else {
            parent.join(format!("{stem}-{}.{}", index + 1, image.extension))
        };
        let format = Format {
            id: format!("image-{}", index + 1),
            kind: crate::model::StreamKind::Muxed,
            container: if image.extension == "png" {
                Container::Unknown
            } else {
                Container::Jpeg
            },
            codec: String::new(),
            width: 0,
            height: 0,
            fps: 0,
            bitrate: 0,
            bytes: None,
            url: Some(image.url.clone()),
            headers: image.headers.clone(),
            source: "image",
        };
        let bytes = fetch_stream(http, &format, &path, options, printer)?;
        commit(&part_path(&path), &path, options.overwrite)?;
        total += bytes;
        if images.len() > 1 {
            announce_path(&path, options, printer);
        }
    }
    Ok(total)
}

/// Fetches one stream into `<target>.part` and leaves it there.
fn fetch_stream(
    http: &Http,
    format: &Format,
    target: &Path,
    options: &Options,
    printer: &Printer,
) -> Result<u64> {
    let part = part_path(target);
    fetch_stream_to(http, format, &part, options, printer)
}

fn fetch_stream_to(
    http: &Http,
    format: &Format,
    part: &Path,
    options: &Options,
    printer: &Printer,
) -> Result<u64> {
    let url = format
        .url
        .clone()
        .ok_or_else(|| Error::Internal("the chosen format has no URL".into()))?;

    let headers = request_headers(format);
    let borrowed: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();

    let mut resume_from = if options.resume {
        match fs::metadata(part) {
            Ok(metadata) if metadata.is_file() => metadata.len(),
            _ => 0,
        }
    } else {
        0
    };
    if resume_from > 0 {
        printer.note(&format!(
            "resuming {} at {}",
            part.display(),
            human_bytes(resume_from)
        ));
    }

    let mut progress = printer.progress(
        format!("{} {}", format.label(), format.container_extension()),
        format.bytes,
    );
    progress.add(resume_from);

    let mut file = open_part(part, resume_from > 0)?;
    let mut report = http.download(
        "media",
        &url,
        &borrowed,
        &mut file,
        resume_from,
        &mut |bytes| progress.add(bytes),
    )?;

    if report.restarted {
        // The server ignored the Range request: start over rather than writing
        // the beginning of the file twice.
        drop(file);
        file = open_part(part, false)?;
        resume_from = 0;
        report = http.download(
            "media",
            &url,
            &borrowed,
            &mut file,
            resume_from,
            &mut |bytes| progress.add(bytes),
        )?;
        if report.restarted {
            return Err(Error::Http(
                "the server keeps ignoring a range request; cannot resume".into(),
            ));
        }
    }

    file.flush()?;
    file.sync_all()?;
    drop(file);

    let written = fs::metadata(part)?.len();
    if let Some(expected) = report.total {
        if written != expected {
            return Err(Error::Verify(format!(
                "{}: expected {expected} bytes, received {written}",
                part.display()
            )));
        }
    }
    if written == 0 {
        return Err(Error::Http(format!(
            "{}: the stream was empty",
            part.display()
        )));
    }

    progress.clear();
    Ok(written)
}

fn open_part(path: &Path, append: bool) -> Result<File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(paths::verbatim(parent))?;
        }
    }
    // `paths::verbatim` is the identity everywhere but Windows, where it is what
    // lets a deep output directory hold a file at all.
    let path = paths::verbatim(path);
    if append {
        OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(Error::Io)
    } else {
        File::create(path).map_err(Error::Io)
    }
}

/// Renames a finished temporary into place, after checking that nothing is
/// going to be replaced silently.
fn commit(part: &Path, target: &Path, overwrite: bool) -> Result<()> {
    if target.exists() && !overwrite {
        return Err(Error::Refused(format!(
            "{} already exists; pass --overwrite to replace it",
            target.display()
        )));
    }
    fs::rename(paths::verbatim(part), paths::verbatim(target)).map_err(|e| {
        Error::Io(io::Error::new(
            e.kind(),
            format!(
                "could not rename {} to {}: {e}",
                part.display(),
                target.display()
            ),
        ))
    })?;
    sync_directory(target);
    Ok(())
}

/// Makes the rename durable. Failure is not fatal: on Windows a directory
/// cannot be opened as a file at all, and on Unix an fsync that fails here has
/// already had its rename applied.
fn sync_directory(target: &Path) {
    let parent = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    if let Ok(handle) = File::open(parent) {
        let _ = handle.sync_all();
    }
}

/// `-o -`: the media goes to stdout instead of a file.
fn is_stdout(path: &Path) -> bool {
    path.as_os_str() == std::ffi::OsStr::new("-")
}

fn part_path(target: &Path) -> PathBuf {
    suffix_path(target, ".part")
}

fn suffix_path(target: &Path, suffix: &str) -> PathBuf {
    let mut text = target.as_os_str().to_os_string();
    text.push(suffix);
    PathBuf::from(text)
}

/// `clip.mp4` → `clip.audio.m4a`.
fn audio_path(video_target: &Path, audio: &Format) -> PathBuf {
    let extension = match audio.container {
        Container::Mp4 => "m4a",
        other => other.extension(),
    };
    let stem = video_target
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audio".to_string());
    let parent = video_target.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{stem}.audio.{extension}"))
}

fn request_headers(format: &Format) -> Vec<(String, String)> {
    let mut headers = vec![(
        "user-agent".to_string(),
        crate::http::UA_DESKTOP.to_string(),
    )];
    headers.push(("accept".to_string(), "*/*".to_string()));
    for (name, value) in &format.headers {
        headers.push((name.clone(), value.clone()));
    }
    headers
}

fn print_formats(media: &Media, printer: &Printer) -> Result<()> {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {} — {}\n",
        media.site,
        media.id,
        media.display_title()
    ));
    out.push_str(
        "  id                          kind    container  codec               quality   size\n",
    );
    for format in &media.formats {
        let kind = match format.kind {
            crate::model::StreamKind::Muxed => "muxed",
            crate::model::StreamKind::Video => "video",
            crate::model::StreamKind::Audio => "audio",
        };
        let container = match format.container {
            Container::Mp4 => "mp4",
            Container::Webm => "webm",
            Container::Ts => "ts",
            Container::Hls => "hls",
            Container::Jpeg => "jpg",
            Container::Mp3 => "mp3",
            Container::Unknown => "?",
        };
        let size = match format.bytes {
            Some(bytes) => human_bytes(bytes),
            None => format
                .estimate_bytes(media.duration.unwrap_or(0))
                .map(human_bytes)
                .unwrap_or_else(|| "?".to_string()),
        };
        out.push_str(&format!(
            "  {:<26}  {:<6}  {:<8}  {:<18}  {:<7}  {}\n",
            format.id,
            kind,
            container,
            truncate(&format.codec, 18),
            format.label(),
            size
        ));
    }
    for (index, image) in media.images.iter().enumerate() {
        out.push_str(&format!(
            "  image-{:<20}  photo  {:<8}  {:<18}\n",
            index + 1,
            image.extension,
            ""
        ));
    }
    if media.formats.is_empty() && media.images.is_empty() {
        out.push_str("  (no streams)\n");
    }
    let _ = printer;
    if !write_stdout(&out) {
        return Ok(());
    }
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars()
            .take(max.saturating_sub(1))
            .chain(['…'])
            .collect()
    }
}

impl Format {
    fn container_extension(&self) -> &'static str {
        self.container.extension()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_paths_sit_next_to_the_target() {
        assert_eq!(
            part_path(Path::new("/tmp/a.mp4")),
            PathBuf::from("/tmp/a.mp4.part")
        );
        assert_eq!(
            suffix_path(Path::new("/tmp/a.mp4"), ".audio.part"),
            PathBuf::from("/tmp/a.mp4.audio.part")
        );
    }

    #[test]
    fn the_audio_side_of_a_separate_pair_is_named_after_the_video() {
        let format = Format {
            id: "1".into(),
            kind: crate::model::StreamKind::Audio,
            container: Container::Mp4,
            codec: "mp4a".into(),
            width: 0,
            height: 0,
            fps: 0,
            bitrate: 0,
            bytes: None,
            url: None,
            headers: Vec::new(),
            source: "test",
        };
        assert_eq!(
            audio_path(Path::new("out/clip.mp4"), &format),
            PathBuf::from("out/clip.audio.m4a")
        );
    }

    #[test]
    fn a_missing_directory_is_created_for_the_output() {
        let dir = std::env::temp_dir().join(format!("tafas-target-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A path that does not exist and has no trailing slash names a file, so
        // the slash is what makes this a directory.
        let options = Options {
            output: Some(format!("{}/", dir.display()).into()),
            ..Options::default()
        };

        let media = Media::new(
            crate::model::Site::YouTube,
            "dQw4w9WgXcQ".into(),
            "https://example".into(),
        );
        let format = Format {
            id: "137".into(),
            kind: crate::model::StreamKind::Video,
            container: Container::Mp4,
            codec: "avc1".into(),
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 0,
            bytes: None,
            url: None,
            headers: Vec::new(),
            source: "test",
        };
        let path = target_path(&options, &media, &Plan::Single(format)).unwrap();
        assert!(dir.is_dir());
        assert!(path.starts_with(&dir));
        assert_eq!(path.file_name().unwrap(), "dQw4w9WgXcQ.mp4");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_refuses_to_replace_without_permission() {
        let dir = std::env::temp_dir().join(format!("tafas-commit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.mp4");
        let part = dir.join("a.mp4.part");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&part, b"new").unwrap();

        assert!(commit(&part, &target, false).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(part.exists());

        commit(&part, &target, true).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!part.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stdout_target_is_stderr_free() {
        let options = Options {
            output: Some(PathBuf::from("-")),
            ..Options::default()
        };
        let media = Media::new(crate::model::Site::X, "20".into(), "https://example".into());
        let format = Format {
            id: "1".into(),
            kind: crate::model::StreamKind::Muxed,
            container: Container::Mp4,
            codec: String::new(),
            width: 0,
            height: 0,
            fps: 0,
            bitrate: 0,
            bytes: None,
            url: None,
            headers: Vec::new(),
            source: "test",
        };
        assert_eq!(
            target_path(&options, &media, &Plan::Single(format)).unwrap(),
            PathBuf::from("-")
        );
    }
}
