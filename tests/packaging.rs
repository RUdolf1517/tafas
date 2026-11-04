//! Packaging invariants: the manual page, the completions and the parser must
//! describe the same program, and the program must not grow a way to run another
//! one.
//!
//! These are cheap checks that catch a class of bug no unit test can: a flag that
//! exists in `--help` and nowhere else, a manual page that drifted a release ago,
//! a `Command::new` that quietly turned a static binary into a wrapper.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = manifest_dir().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn help_text() -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_tafas"))
        .arg("--help")
        .output()
        .expect("the binary runs");
    assert!(output.status.success(), "--help failed");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Every long option mentioned in `--help`, without the `--`.
fn help_long_options(help: &str) -> BTreeSet<String> {
    let mut options = BTreeSet::new();
    for token in help.split(|c: char| c.is_whitespace() || c == ',' || c == '[' || c == ']') {
        if let Some(name) = token.strip_prefix("--") {
            let name = name.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-');
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '-') {
                options.insert(name.to_string());
            }
        }
    }
    options
}

#[test]
fn every_option_in_help_appears_in_the_manual_page() {
    let help = help_text();
    // groff escapes every hyphen (`\-`), including the ones inside a word, so
    // the page is normalised before it is compared with the parser's output.
    let man = read("man/tafas.1").replace("\\-", "-");

    let missing: Vec<String> = help_long_options(&help)
        .into_iter()
        // `--` itself is punctuation, not an option.
        .filter(|name| !name.is_empty())
        .filter(|name| !man.contains(&format!("--{name}")))
        .collect();

    assert!(
        missing.is_empty(),
        "options in --help but not in man/tafas.1: {missing:?}"
    );
}

#[test]
fn the_manual_page_names_the_current_version() {
    let version = env!("CARGO_PKG_VERSION");
    let man = read("man/tafas.1");
    assert!(
        man.contains(&format!("tafas {version}")),
        "man/tafas.1 does not mention version {version}"
    );
}

#[test]
fn the_binary_runs_no_other_program() {
    let sources = source_files(&manifest_dir().join("src"));
    assert!(!sources.is_empty(), "no source files found");
    for path in sources {
        let text = std::fs::read_to_string(&path).expect("read a source file");
        assert!(
            !text.contains("Command::new"),
            "{} spawns a subprocess; tafas is one static binary and runs nothing else",
            path.display()
        );
        assert!(
            !text.contains("unsafe {"),
            "{} contains unsafe code, and the crate forbids it",
            path.display()
        );
    }
}

fn source_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return files,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(source_files(&path));
        } else if path.extension().map(|ext| ext == "rs").unwrap_or(false) {
            files.push(path);
        }
    }
    files
}

#[test]
fn the_readme_documents_the_exit_codes_the_binary_uses() {
    let readme = read("README.md");
    for code in ["`0`", "`1`", "`2`", "`3`", "`4`"] {
        assert!(
            readme.contains(code),
            "README does not document exit code {code}"
        );
    }
}

#[test]
fn the_documented_sites_are_the_ones_compiled_in() {
    let capabilities = Command::new(env!("CARGO_BIN_EXE_tafas"))
        .arg("--capabilities")
        .output()
        .expect("the binary runs");
    let value: serde_json::Value =
        serde_json::from_slice(&capabilities.stdout).expect("capabilities is JSON");
    let sites: Vec<&str> = value["sites"]
        .as_array()
        .expect("sites array")
        .iter()
        .map(|site| site.as_str().unwrap_or(""))
        .collect();

    let readme = read("README.md");
    for site in sites {
        assert!(
            readme.to_lowercase().contains(site),
            "README does not mention the {site} extractor"
        );
    }
}

#[test]
fn the_dependency_list_in_the_docs_matches_the_manifest() {
    let cargo = read("Cargo.toml");
    let docs = read("docs/dependencies.md");
    for crate_name in ["clap", "clap_complete", "reqwest", "rustls", "serde_json"] {
        assert!(
            cargo.contains(&format!("{crate_name} =")),
            "{crate_name} is not a dependency any more; update docs/dependencies.md and this test"
        );
        assert!(
            docs.contains(crate_name),
            "{crate_name} is not documented in docs/dependencies.md"
        );
    }
}
