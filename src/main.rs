#![forbid(unsafe_code)]

//! The binary: parse arguments, answer the informational flags, run the work.
//!
//! Exit codes are the contract in `docs/cli.md` §5. The mapping from an error to
//! a code lives in `error.rs`; the mapping from a *run* to a code lives in
//! `report.rs`. Nothing here decides either.

use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use serde_json::json;

use tafas::cli::Args;
use tafas::download::{self, write_stdout};
use tafas::report::RESULT_SCHEMA_VERSION;
use tafas::sites::SITES;
use tafas::style::{self, Pen};

fn main() -> ExitCode {
    // Installing the provider explicitly is what makes the dependency choice
    // real: rustls is built with `ring`, and a build that forgot to say so
    // would fail here rather than silently pulling in another backend.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = Args::parse();

    if args.man {
        return emit(include_str!("../man/tafas.1"));
    }
    if let Some(shell) = args.completions {
        let mut command = Args::command();
        clap_complete::generate(shell, &mut command, "tafas", &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }
    if args.capabilities {
        return emit(&format!("{}\n", capabilities()));
    }
    if args.info {
        return emit(&info(&args));
    }

    if args.urls.is_empty() {
        usage(&args, "no URL given");
        return ExitCode::from(2);
    }
    if args.sub_langs.is_some() && !args.write_subs {
        usage(
            &args,
            "--sub-langs chooses which caption tracks to write, so it needs --write-subs",
        );
        return ExitCode::from(2);
    }
    if args.format.is_some() && args.urls.len() > 1 {
        usage(
            &args,
            "--format selects one stream and needs exactly one URL",
        );
        return ExitCode::from(2);
    }

    let options = match args.options() {
        Ok(options) => options,
        Err(error) => {
            usage(&args, &error.to_string());
            return ExitCode::from(2);
        }
    };
    let code = download::run(&args.urls, &options);
    ExitCode::from(code)
}

fn emit(text: &str) -> ExitCode {
    // A closed pipe is not an error: `tafas --man | head -1` must be quiet and
    // successful, the way every other Unix filter is.
    let _ = write_stdout(text);
    ExitCode::SUCCESS
}

fn usage(args: &Args, message: &str) {
    let pen = Pen::for_stderr(args.color());
    eprintln!("{}", pen.paint(style::RED, &format!("tafas: {message}")));
    eprintln!("usage: tafas [OPTIONS] <URL>...");
    eprintln!(
        "{}",
        pen.paint(style::DIM, "try `tafas --help` for the full list")
    );
}

/// A description of what this build can do, for scripts and for bug reports.
///
/// It is read-only: it makes no request and touches no file. The two version
/// numbers are separate on purpose — the capability document and the per-item
/// result document are different contracts with different lifetimes.
fn capabilities() -> String {
    let value = json!({
        "capabilities_schema_version": 1,
        "version": env!("CARGO_PKG_VERSION"),
        "result_schema_version": RESULT_SCHEMA_VERSION,
        "sites": SITES,
        "extractors": {
            "youtube": {
                "routes": ["watch page (ytInitialPlayerResponse)", "innertube"],
                "clients": ["android_vr", "ios", "android", "tv_embedded", "web"],
                "signature_decipher": false,
                "po_token": false,
                "cookies": "user-supplied (--cookies FILE)"
            },
            "tiktok": {
                "routes": ["web page (__UNIVERSAL_DATA_FOR_REHYDRATION__)", "mobile feed"],
                "watermark": "avoided by preferring playAddr",
                "cookies": "user-supplied (--cookies FILE)"
            },
            "x": {
                "routes": ["syndication tweet-result"],
                "hls": true,
                "cookies": "user-supplied (--cookies FILE)"
            }
        },
        "extractor_args": {
            "youtube:client": crate::extractor_args::YOUTUBE_CLIENTS,
            "tiktok:route": crate::extractor_args::TIKTOK_ROUTES
        },
        "containers": ["mp4", "m4a"],
        "hls": {
            "playlists": ["master", "media"],
            "segments": ["mpeg-ts", "fmp4"],
            "encryption": ["aes-128"],
            "fmp4": true,
            "live": false
        },
        "muxing": {
            "container": "mp4",
            "reencode": false,
            "fragmented_input": false,
            "verified": true
        },
        "transfers": {
            "range_resume": true,
            "atomic_commit": true,
            "retries_default": 3,
            "max_redirects": 10,
            "concurrent_fragments": { "default": 1, "max": 16 }
        },
        "external_binaries": [],
        "tls": "rustls (ring provider)",
        "telemetry": false,
        "configuration_files": false
    });
    value.to_string()
}

fn info(args: &Args) -> String {
    let pen = Pen::for_stdout(args.color());
    let mut out = String::new();
    out.push_str(&format!(
        "{} {}\n",
        pen.paint(style::BOLD, "tafas"),
        env!("CARGO_PKG_VERSION")
    ));
    out.push_str("capture YouTube, TikTok and X as one playable MP4, without re-encoding\n\n");
    out.push_str(&format!("sites        {}\n", SITES.join(", ")));
    out.push_str("muxing       mp4 (streams copied, index rebuilt, output verified)\n");
    out.push_str("tls          rustls with the ring provider\n");
    out.push_str("external     none — no yt-dlp, no ffmpeg, no shelling out\n");
    out.push_str("config       none; every decision is an argument\n");
    out.push_str("telemetry    none\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_is_valid_json_with_both_versions() {
        let value: serde_json::Value = serde_json::from_str(&capabilities()).unwrap();
        assert_eq!(value["capabilities_schema_version"], 1);
        assert_eq!(value["result_schema_version"], RESULT_SCHEMA_VERSION);
        assert_eq!(value["sites"].as_array().unwrap().len(), 3);
        assert_eq!(value["muxing"]["reencode"], false);
        assert_eq!(value["external_binaries"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn info_names_the_sites_and_the_absence_of_external_tools() {
        let args = Args::try_parse_from(["tafas", "--info"]).unwrap();
        let text = info(&args);
        assert!(text.contains("youtube"));
        assert!(text.contains("none"));
    }
}
