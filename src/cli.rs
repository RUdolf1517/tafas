//! The command line, in one place.
//!
//! The parser is `clap` with derive, which gives GNU-style behaviour for free:
//! `--long`, `-s`, `--`, `--opt=value` and `--opt value` all work, `-vv` counts,
//! an unknown flag is exit 2 with a suggestion, and `--help` goes to stdout
//! while errors go to stderr. The *meaning* of every flag lives in
//! `docs/cli.md`; this file only declares them, so the manual page, the
//! completions and `--help` cannot drift apart.

use std::path::PathBuf;

use clap::Parser;

use crate::download::Options;
use crate::select::Quality;
use crate::style::Choice;

const LONG_ABOUT: &str = "\
Capture a video from YouTube, TikTok or X as one playable file.

tafas reads the page, picks the best stream the site will hand out, downloads it
and — when the site serves picture and sound separately, as YouTube does above
720p — joins them into one MP4 without re-encoding. Nothing is transcoded:
frames and samples are copied, and only the container's index is rebuilt.

Everything tafas writes goes through a .part file next to the destination and is
renamed into place only after it has been verified.";

const AFTER_HELP: &str = "\
Exit codes:
  0  success
  1  at least one URL failed; the others were still captured
  2  invalid usage; nothing was requested
  3  fatal error; the run could not be completed
  4  an output file failed verification — this is a bug, not bad luck
 130  interrupted (SIGINT); a .part file may remain and the next run picks it up

stdout is data: the written path, one per line. stderr is everything else.
A closed stdout pipe ends the run quietly and successfully.";

#[derive(Debug, Parser)]
#[command(
    name = "tafas",
    version,
    about = "Capture a video from YouTube, TikTok or X as one playable MP4",
    long_about = LONG_ABOUT,
    after_help = AFTER_HELP,
    disable_help_subcommand = true,
    arg_required_else_help = true,
)]
pub struct Args {
    /// Page URLs to capture
    #[arg(value_name = "URL")]
    pub urls: Vec<String>,

    /// Output file, or directory when several URLs are given ("-" = stdout)
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Highest resolution to take: best, worst, or a height such as 720p
    #[arg(
        short = 'Q',
        long,
        value_name = "QUALITY",
        default_value = "best",
        value_parser = parse_quality
    )]
    pub quality: Quality,

    /// Audio only, as m4a when the site offers one
    #[arg(long)]
    pub audio_only: bool,

    /// Write a <name>.info.json beside each file: what was chosen, and from what
    #[arg(long)]
    pub write_info_json: bool,

    /// Write the page's poster frame beside each file
    #[arg(long)]
    pub write_thumbnail: bool,

    /// Write the page's caption tracks beside each file, as WebVTT
    #[arg(long)]
    pub write_subs: bool,

    /// Which caption languages to write: en, ru, pt-BR, en*, or all (default all)
    #[arg(long, value_name = "LANGS", value_parser = parse_sub_langs)]
    pub sub_langs: Option<crate::subtitles::Selection>,

    /// Exact stream id to fetch, as printed by --list-formats
    #[arg(long, value_name = "ID")]
    pub format: Option<String>,

    /// Keep picture and sound in separate files; never remux
    #[arg(long)]
    pub no_mux: bool,

    /// Replace an existing file
    #[arg(long)]
    pub overwrite: bool,

    /// Do not continue an interrupted download
    #[arg(long)]
    pub no_resume: bool,

    /// Use this name stem instead of "<title> [<id>]"
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Print what would be fetched and where, then stop
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// List every stream of each URL, then stop
    #[arg(long)]
    pub list_formats: bool,

    /// One JSON object per item on stdout
    #[arg(long)]
    pub json: bool,

    /// Errors only
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Never draw the progress line
    #[arg(long)]
    pub no_progress: bool,

    /// How many URLs to fetch at once (0 = four)
    #[arg(short = 'j', long, value_name = "N", default_value_t = 1)]
    pub jobs: usize,

    /// Retries for one failed request
    #[arg(long, value_name = "N", default_value_t = 3)]
    pub retries: u32,

    /// Timeout for one request, in seconds
    #[arg(long, value_name = "SECS", default_value_t = 60)]
    pub timeout: u64,

    /// Take only these segments of an HLS list: 3, 1-5, 2- (1-based)
    #[arg(long, value_name = "ITEMS", value_parser = parse_items)]
    pub playlist_items: Option<crate::items::Items>,

    /// Fetch this many HLS segments at once (1 = one after another)
    #[arg(
        long,
        value_name = "N",
        default_value_t = 1,
        value_parser = parse_concurrency
    )]
    pub concurrent_fragments: usize,

    /// Send cookies from a Netscape cookie file (read once, never written back)
    #[arg(long, value_name = "FILE")]
    pub cookies: Option<PathBuf>,

    /// Narrow an extractor's route: SITE:key=value (repeatable)
    #[arg(
        long,
        value_name = "SITE:KEY=VALUE",
        value_parser = parse_extractor_args,
        action = clap::ArgAction::Append
    )]
    pub extractor_args: Vec<crate::extractor_args::ExtractorArgs>,

    /// Log requests and decisions on stderr (repeatable)
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Colour: auto, always, never
    #[arg(
        long,
        value_name = "WHEN",
        default_value = "auto",
        value_parser = parse_color
    )]
    pub color: Choice,

    /// Same as --color never
    #[arg(long)]
    pub no_color: bool,

    /// Print a shell completion script and exit
    #[arg(long, value_name = "SHELL", value_enum)]
    pub completions: Option<clap_complete::Shell>,

    /// Print the manual page (groff) and exit
    #[arg(long)]
    pub man: bool,

    /// Print a JSON description of this build and exit
    #[arg(long)]
    pub capabilities: bool,

    /// Print version, supported sites and defaults
    #[arg(short = 'i', long)]
    pub info: bool,
}

fn parse_quality(text: &str) -> Result<Quality, String> {
    Quality::parse(text).map_err(|error| error.to_string())
}

fn parse_sub_langs(text: &str) -> Result<crate::subtitles::Selection, String> {
    crate::subtitles::Selection::parse(text).map_err(|error| error.to_string())
}

fn parse_items(text: &str) -> Result<crate::items::Items, String> {
    crate::items::Items::parse(text).map_err(|error| error.to_string())
}

fn parse_color(text: &str) -> Result<Choice, String> {
    Choice::parse(text)
}

/// clap wants a message here, not an [`crate::error::Error`], so the check is
/// done once in `extractor_args` and the text is reused.
fn parse_extractor_args(text: &str) -> Result<crate::extractor_args::ExtractorArgs, String> {
    crate::extractor_args::ExtractorArgs::parse(text).map_err(|error| error.to_string())
}

/// `--concurrent-fragments N`: at least one, and no more than the fetch path
/// will actually run — a number the program silently reduced would be a lie
/// about what it did.
fn parse_concurrency(text: &str) -> Result<usize, String> {
    let limit = crate::hlsfetch::CONCURRENCY_LIMIT;
    let value: usize = text.trim().parse().map_err(|_| {
        format!("--concurrent-fragments takes a whole number between 1 and {limit}, not {text:?}")
    })?;
    if value == 0 || value > limit {
        return Err(format!(
            "--concurrent-fragments must be between 1 and {limit}, not {value}"
        ));
    }
    Ok(value)
}

impl Args {
    /// `--extractor-args` folded into one table.
    ///
    /// A key named twice is a usage error rather than a silent last-one-wins:
    /// the flag is repeatable so that each site gets its own occurrence, not so
    /// that two of them can disagree.
    fn extractor_args(&self) -> crate::error::Result<crate::extractor_args::ExtractorArgs> {
        let mut folded = crate::extractor_args::ExtractorArgs::default();
        for args in &self.extractor_args {
            folded.absorb(args.clone())?;
        }
        Ok(folded)
    }

    /// The colour decision after `--no-color` has had its say.
    pub fn color(&self) -> Choice {
        if self.no_color {
            Choice::Never
        } else {
            self.color
        }
    }

    /// The number of URLs to work on at once. `0` is "four", which is enough to
    /// hide the latency of one page fetch behind another without opening a
    /// connection per item to a site that answers 429 above a modest rate.
    pub fn jobs(&self) -> usize {
        if self.jobs == 0 {
            4
        } else {
            self.jobs
        }
    }

    pub fn options(&self) -> crate::error::Result<Options> {
        Ok(Options {
            output: self.output.clone(),
            multiple: self.urls.len() > 1,
            quality: self.quality,
            audio_only: self.audio_only,
            write_info_json: self.write_info_json,
            write_thumbnail: self.write_thumbnail,
            write_subs: self.write_subs,
            sub_langs: self.sub_langs.clone(),
            format: self.format.clone(),
            no_mux: self.no_mux,
            overwrite: self.overwrite,
            resume: !self.no_resume,
            name: self.name.clone(),
            dry_run: self.dry_run,
            list_formats: self.list_formats,
            json: self.json,
            quiet: self.quiet,
            no_progress: self.no_progress,
            jobs: self.jobs(),
            retries: self.retries,
            timeout: self.timeout,
            cookies: self.cookies.clone(),
            extractor_args: self.extractor_args()?,
            playlist_items: self.playlist_items.clone(),
            concurrent_fragments: self.concurrent_fragments,
            verbose: self.verbose,
            color: self.color(),
        })
    }

    /// Flags that answer a question and exit, so a missing URL is not an error.
    pub fn is_informational(&self) -> bool {
        self.man || self.capabilities || self.info || self.completions.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_parser_is_consistent() {
        // Fails if two flags collide, which is the mistake this saves most.
        Args::command().debug_assert();
    }

    #[test]
    fn defaults_match_the_documented_ones() {
        let args = Args::parse_from(["tafas", "https://youtu.be/dQw4w9WgXcQ"]);
        let options = args.options().unwrap();
        assert_eq!(options.quality, Quality::Best);
        assert!(options.resume);
        assert!(!options.overwrite);
        assert_eq!(options.jobs, 1);
        assert_eq!(options.retries, 3);
        assert_eq!(options.timeout, 60);
        assert_eq!(options.color, Choice::Auto);
    }

    #[test]
    fn quality_and_colour_are_parsed() {
        let args = Args::parse_from([
            "tafas",
            "--quality",
            "720p",
            "--color",
            "never",
            "https://x.com/a/status/1",
        ]);
        assert_eq!(args.options().unwrap().quality, Quality::Height(720));
        assert_eq!(args.color(), Choice::Never);
    }

    #[test]
    fn jobs_zero_means_four() {
        let args = Args::parse_from(["tafas", "-j", "0", "https://youtu.be/dQw4w9WgXcQ"]);
        assert_eq!(args.jobs(), 4);
        assert!(!args.options().unwrap().multiple);
    }

    #[test]
    fn several_urls_mark_the_output_as_a_directory() {
        let args = Args::parse_from([
            "tafas",
            "-o",
            "out/",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://x.com/a/status/1",
        ]);
        assert!(args.options().unwrap().multiple);
    }

    #[test]
    fn no_color_overrides_the_colour_choice() {
        let args = Args::parse_from([
            "tafas",
            "--color",
            "always",
            "--no-color",
            "https://youtu.be/dQw4w9WgXcQ",
        ]);
        assert_eq!(args.color(), Choice::Never);
    }

    #[test]
    fn an_invalid_quality_is_a_usage_error() {
        let error =
            Args::try_parse_from(["tafas", "--quality", "huge", "https://youtu.be/dQw4w9WgXcQ"])
                .unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn no_arguments_prints_help_and_exits_two() {
        let error = Args::try_parse_from(["tafas"]).unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }
}
