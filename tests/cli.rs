//! The binary, spawned, as a user or a script would.
//!
//! Anything that needs a page on the internet is covered by unit tests over the
//! documents the extractors read; this file covers the promises the *shell*
//! depends on: which stream gets which text, and which failure gets which exit
//! code.

use std::process::{Command, Output};

fn tafas(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tafas"))
        .args(args)
        .env_remove("NO_COLOR")
        .env("TERM", "dumb")
        .output()
        .expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

#[test]
fn version_goes_to_stdout_and_exits_zero() {
    let output = tafas(&["--version"]);
    assert_eq!(code(&output), 0);
    assert!(stdout(&output).starts_with("tafas "));
    assert!(stderr(&output).is_empty());
}

#[test]
fn help_goes_to_stdout_and_lists_the_exit_codes() {
    let output = tafas(&["--help"]);
    assert_eq!(code(&output), 0);
    let text = stdout(&output);
    assert!(text.contains("--quality"));
    assert!(text.contains("--list-formats"));
    assert!(text.contains("Exit codes"));
    assert!(stderr(&output).is_empty());
}

#[test]
fn no_arguments_is_a_usage_error_on_stderr() {
    let output = tafas(&[]);
    assert_eq!(code(&output), 2);
    assert!(stdout(&output).is_empty());
    let text = stderr(&output);
    assert!(text.contains("Usage") || text.contains("usage"));
}

#[test]
fn an_invalid_quality_is_rejected_before_anything_is_requested() {
    let output = tafas(&["--quality", "huge", "https://youtu.be/dQw4w9WgXcQ"]);
    assert_eq!(code(&output), 2);
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).to_lowercase().contains("quality"));
}

#[test]
fn a_host_with_no_extractor_fails_with_one_line_and_exit_one() {
    let output = tafas(&["--timeout", "1", "https://example.invalid/video"]);
    assert_eq!(code(&output), 1);
    assert!(stdout(&output).is_empty());
    let text = stderr(&output);
    assert!(text.starts_with("tafas: "), "got {text:?}");
    assert!(text.contains("no extractor"), "got {text:?}");
}

#[test]
fn json_failures_are_one_object_on_stdout() {
    let output = tafas(&["--json", "https://example.invalid/video"]);
    assert_eq!(code(&output), 1);
    let text = stdout(&output);
    let line = text.lines().next().expect("one JSON line");
    let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
    assert_eq!(value["schema"], 1);
    assert_eq!(value["status"], "failed");
    assert_eq!(value["reason"], "unsupported");
    assert_eq!(value["path"], serde_json::Value::Null);
}

#[test]
fn capabilities_is_read_only_json() {
    let output = tafas(&["--capabilities"]);
    assert_eq!(code(&output), 0);
    let value: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("valid JSON");
    assert_eq!(value["capabilities_schema_version"], 1);
    assert_eq!(
        value["external_binaries"].as_array().expect("array").len(),
        0
    );
    assert_eq!(value["muxing"]["reencode"], false);
}

#[test]
fn completions_are_generated_for_every_supported_shell() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = tafas(&["--completions", shell]);
        assert_eq!(code(&output), 0, "{shell} failed: {}", stderr(&output));
        assert!(!stdout(&output).is_empty(), "{shell} produced nothing");
    }
}

#[test]
fn the_manual_page_is_printed_on_stdout() {
    let output = tafas(&["--man"]);
    assert_eq!(code(&output), 0);
    let text = stdout(&output);
    assert!(text.contains(".TH TAFAS 1"));
    assert!(text.contains(".SH OPTIONS"));
}

#[test]
fn info_names_the_sites() {
    let output = tafas(&["--info"]);
    assert_eq!(code(&output), 0);
    let text = stdout(&output);
    for site in ["youtube", "tiktok", "x", "vimeo", "reddit", "soundcloud"] {
        assert!(text.contains(site), "{site} missing from --info");
    }
}

#[test]
fn format_requires_exactly_one_url() {
    let output = tafas(&[
        "--format",
        "137",
        "https://youtu.be/dQw4w9WgXcQ",
        "https://youtu.be/aqz-KE-bpKQ",
    ]);
    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("--format"));
}

#[test]
fn sub_langs_without_write_subs_is_a_usage_error() {
    let output = tafas(&["--sub-langs", "en", "https://youtu.be/dQw4w9WgXcQ"]);
    assert_eq!(
        code(&output),
        2,
        "a flag that would do nothing is a mistake"
    );
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("--write-subs"));
}

#[test]
fn write_subs_and_embed_subs_together_is_a_usage_error() {
    let output = tafas(&[
        "--write-subs",
        "--embed-subs",
        "https://youtu.be/dQw4w9WgXcQ",
    ]);
    assert_eq!(code(&output), 2, "one asks for a file, the other a track");
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("--embed-subs"));
}

#[test]
fn a_malformed_sub_langs_list_is_rejected_before_any_request() {
    let output = tafas(&[
        "--write-subs",
        "--sub-langs",
        "en,,ru",
        "https://youtu.be/dQw4w9WgXcQ",
    ]);
    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("language"));
}

#[test]
fn a_trailing_dash_argument_is_treated_as_a_url() {
    // `--` must let a value that looks like a flag through untouched, and the
    // result is then a usage-level rejection rather than a parse error in the
    // wrong place.
    let output = tafas(&["--", "-not-a-url"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).starts_with("tafas: "));
}

#[test]
fn color_never_and_no_color_agree() {
    let a = tafas(&["--color", "never", "https://example.invalid/v"]);
    let b = tafas(&["--no-color", "https://example.invalid/v"]);
    assert_eq!(stderr(&a), stderr(&b));
    assert!(!stderr(&a).contains('\u{1b}'));
}

#[test]
fn a_missing_cookie_file_is_reported_before_any_request() {
    let output = tafas(&[
        "--cookies",
        "/nonexistent/tafas/cookies.txt",
        "https://youtu.be/dQw4w9WgXcQ",
    ]);
    assert_eq!(code(&output), 1);
    let text = stderr(&output);
    assert!(text.contains("cookies.txt"), "{text}");
    assert!(text.starts_with("tafas: "), "{text}");
}

#[test]
fn a_cookie_file_with_no_cookies_in_it_is_a_usage_error() {
    let path = std::env::temp_dir().join(format!("tafas-cli-cookies-{}", std::process::id()));
    std::fs::write(&path, "{\"cookies\": []}\n").expect("write the file");

    let output = tafas(&[
        "--cookies",
        path.to_str().expect("a utf-8 path"),
        "https://youtu.be/dQw4w9WgXcQ",
    ]);
    assert_eq!(
        code(&output),
        2,
        "a jar with no cookies is a mistake in the arguments"
    );
    assert!(stderr(&output).contains("Netscape"), "{}", stderr(&output));
}

#[test]
fn concurrent_fragments_is_bounded_before_anything_is_requested() {
    for bad in ["0", "17", "four"] {
        let output = tafas(&[
            "--concurrent-fragments",
            bad,
            "https://youtu.be/aqz-KE-bpKQ",
        ]);
        assert_eq!(code(&output), 2, "--concurrent-fragments {bad}");
        assert!(stdout(&output).is_empty());
        let text = stderr(&output);
        assert!(text.contains("--concurrent-fragments"), "{text}");
        assert!(
            text.contains("between 1 and 16"),
            "the bound is named, not just rejected: {text}"
        );
    }
}

#[test]
fn extractor_args_are_checked_against_a_closed_vocabulary() {
    // Every one of these is wrong in a different way and all of them are wrong
    // before a request, so the exit code is the usage one and the message names
    // the spellings that would have worked.
    for bad in [
        "youtube:client=flash",
        "youtube:client",
        "youtube",
        "x:route=web",
        "tiktok:route=mobile",
    ] {
        let output = tafas(&["--extractor-args", bad, "https://youtu.be/aqz-KE-bpKQ"]);
        assert_eq!(code(&output), 2, "--extractor-args {bad}");
        assert!(stdout(&output).is_empty());
        let text = stderr(&output);
        assert!(text.contains("--extractor-args"), "{bad}: {text}");
        assert!(
            text.contains("android_vr"),
            "the refusal lists what is accepted: {bad}: {text}"
        );
    }
}

#[test]
fn extractor_args_are_folded_from_several_flags() {
    // One flag per site is the shape the option is meant to be used in, so both
    // are accepted; the run then stops for the older reason, which is the proof
    // that neither occurrence was rejected.
    let output = tafas(&[
        "--extractor-args",
        "youtube:client=ios",
        "--extractor-args",
        "tiktok:route=app",
    ]);
    assert_eq!(code(&output), 2);
    let text = stderr(&output);
    assert!(text.contains("no URL given"), "{text}");
}

#[test]
fn one_key_named_twice_is_a_usage_error() {
    let output = tafas(&[
        "--extractor-args",
        "youtube:client=ios",
        "--extractor-args",
        "youtube:client=web",
        "https://youtu.be/aqz-KE-bpKQ",
    ]);
    assert_eq!(code(&output), 2);
    assert!(stdout(&output).is_empty());
    let text = stderr(&output);
    assert!(
        text.contains("youtube:client twice"),
        "the duplicate is named, not silently resolved: {text}"
    );
}
