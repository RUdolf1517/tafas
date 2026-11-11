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

/// The Windows asset name appears in four places that must never drift: the
/// installer downloads it, the manifest generator hashes it, the release job
/// builds it, and the documentation tells a reader what to look for.
#[test]
fn the_windows_channel_names_the_asset_the_release_builds() {
    const ASSET: &str = "tafas-x86_64-pc-windows-msvc.zip";
    for file in [
        "packaging/windows/install.ps1",
        "packaging/winget/generate.py",
        ".github/workflows/release.yml",
        "docs/releases.md",
    ] {
        assert!(
            read(file).contains(ASSET),
            "{file} does not name the Windows asset {ASSET}"
        );
    }
}

/// The installer is shipped as an asset and attached to the release, and it must
/// stay a per-user installer: no elevation, a checksum before an unpack, and an
/// uninstall path. A drift here is a change in what installing means.
#[test]
fn the_windows_installer_is_published_and_verifies_what_it_writes() {
    let installer = read("packaging/windows/install.ps1");
    let release = read(".github/workflows/release.yml");

    assert!(
        release.contains("artifacts/install.ps1"),
        "the release job must publish install.ps1 as an asset"
    );
    assert!(
        release.contains("packaging/winget/generate.py"),
        "the release job must render the winget manifests from the shipped ZIP"
    );

    for required in [
        "SHA256SUMS",
        "Get-FileHash",
        "-Uninstall",
        "installer = 'tafas-per-user'",
    ] {
        assert!(
            installer.contains(required),
            "install.ps1 no longer contains {required:?}"
        );
    }
    // No elevation and no machine-wide write: this is a per-user installer.
    for forbidden in [
        "RunAs",
        "EnvironmentVariableTarget]::Machine",
        "ProgramFiles",
        "ProgramData",
    ] {
        assert!(
            !installer.contains(forbidden),
            "install.ps1 mentions {forbidden:?}: this is a per-user installer, not a machine-wide one"
        );
    }
}

/// Every template the generator names must exist and carry the identifier the
/// bundle is filed under, so `winget install --manifest` finds one package.
#[test]
fn the_winget_templates_match_the_generator() {
    let generator = read("packaging/winget/generate.py");
    for template in [
        "version.yaml.in",
        "installer.yaml.in",
        "locale.en-US.yaml.in",
    ] {
        assert!(
            generator.contains(template),
            "generate.py no longer names {template}"
        );
        let path = manifest_dir()
            .join("packaging/winget/templates")
            .join(template);
        assert!(path.is_file(), "{} is missing", path.display());
        let text = read(&format!("packaging/winget/templates/{template}"));
        assert!(
            text.contains("PackageIdentifier: RUdolf1517.tafas"),
            "{template} does not declare the package identifier"
        );
        assert!(
            text.contains("ManifestVersion: 1.6.0"),
            "{template} does not declare the manifest schema version"
        );
    }

    let installer = read("packaging/winget/templates/installer.yaml.in");
    for required in [
        "InstallerType: zip",
        "NestedInstallerType: portable",
        "RelativeFilePath: tafas.exe",
        "PortableCommandAlias: tafas",
    ] {
        assert!(
            installer.contains(required),
            "the installer template no longer declares {required:?}"
        );
    }
}

/// The parts of a release that make it checkable have to stay wired together:
/// the SBOM is built by a script that exists, from the lock file that ships, and
/// the signature step signs what the checksum step wrote and verifies it against
/// a key a reader has. Each of these is one line in a workflow, which is exactly
/// the kind of thing that rots without a test.
#[test]
fn the_release_builds_an_sbom_and_signs_what_it_publishes() {
    let release = read(".github/workflows/release.yml");
    let docs = read("docs/releases.md");

    for required in [
        "packaging/sbom.py",
        "packaging/sign.sh",
        "--version \"$version\"",
        "SOURCE_DATE_EPOCH",
        "--remap-path-prefix",
        "CARGO_INCREMENTAL=0",
    ] {
        assert!(release.contains(required), "release.yml lost {required:?}");
    }
    assert!(
        manifest_dir().join("packaging/sbom.py").is_file(),
        "packaging/sbom.py is named by the release job and does not exist"
    );
    // The signature covers the checksum file and is verified before an upload:
    // signing what was not verified publishes a promise nobody checked.
    let sign = read("packaging/sign.sh");
    assert!(
        sign.contains("SHA256SUMS.minisig"),
        "sign.sh must sign SHA256SUMS"
    );
    assert!(
        sign.contains("minisign -Vm"),
        "sign.sh must verify what it wrote"
    );
    // A key that is committed is the public half; the secret is a secret.
    assert!(
        release.contains("packaging/minisign.pub"),
        "release.yml must check against the published public key"
    );
    assert!(
        docs.contains("minisign.pub") && docs.contains("spdx.json"),
        "docs/releases.md must tell a reader what to verify and how"
    );
    // The Windows ZIP keeps its two entries: the installer and the manifest
    // generator both refuse anything else, so an extra file here is a broken
    // release rather than a richer one.
    let zip_line = release
        .lines()
        .find(|line| line.contains("7z a"))
        .expect("the release job builds the Windows ZIP");
    assert!(
        zip_line.contains("tafas.exe LICENSE"),
        "the Windows ZIP must still carry the binary and the licence: {zip_line}"
    );
    assert!(
        !zip_line.contains(" man") && !zip_line.contains("spdx"),
        "the Windows ZIP must hold exactly two entries: {zip_line}"
    );
}

/// A release is cut only when the roadmap is finished, and the two places that
/// state the rule — the roadmap and the releases page — have to say so.
///
/// The readme used to be checked here too, for a sentence it no longer carries:
/// after 0.2.0 shipped the whole of v0.2–v0.5 the readme tells a reader that
/// 0.2.0 *was* that release, which is the accurate thing for a readme to say. A
/// test that demanded the pre-0.2.0 wording from a document describing 0.2.0 was
/// asking the docs to be wrong, and it failed the moment the docs were read
/// carefully. The rule itself is not weaker for it: the two pages that define
/// when a tag is cut still have to state it.
/// The rule lives in prose, which is exactly the kind of thing that survives one
/// edit and not two.
#[test]
fn no_release_is_cut_before_the_roadmap_is_done() {
    for file in ["docs/roadmap.md", "docs/releases.md"] {
        // Prose wraps: the sentence is checked the way a reader reads it, with
        // line breaks folded into spaces.
        let text = read(file).split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            text.contains("until the whole roadmap is done"),
            "{file} no longer says that nothing is tagged before the roadmap is done"
        );
    }
    // And the procedure itself points at the rule, so nobody follows it in the
    // belief that a version comes out of it today.
    let releases = read("docs/releases.md");
    let cut = releases
        .split("## 4. How a release is cut")
        .nth(1)
        .expect("docs/releases.md describes how a release is cut");
    assert!(
        cut.contains("waits for the roadmap"),
        "the release procedure must say what it is waiting for"
    );
}

/// One page per site, and every site on one of them. The refusal tables are the
/// part of the documentation that grows with each site, and a site added without
/// its page is a refusal a user has to read the source for.
#[test]
fn every_site_has_a_documentation_page() {
    let extractors = read("docs/extractors.md");
    let sites_dir = manifest_dir().join("docs/sites");
    for site in tafas::sites::names() {
        let page = sites_dir.join(format!("{site}.md"));
        assert!(
            page.is_file(),
            "docs/sites/{site}.md is missing for a site this build reads"
        );
        assert!(
            extractors.contains(&format!("sites/{site}.md")),
            "docs/extractors.md does not link docs/sites/{site}.md"
        );
        let text = std::fs::read_to_string(&page).expect("the page reads");
        assert!(
            text.split_whitespace().count() > 100,
            "docs/sites/{site}.md is a stub, not a page"
        );
        // Every markdown link in the page has to resolve: the pages were split
        // out of one file, and a link written when they lived in `docs/` is one
        // directory too shallow once the page is in `docs/sites/`.
        for (at, _) in text.match_indices("](") {
            let rest = &text[at + 2..];
            let Some(close) = rest.find(')') else {
                continue;
            };
            let link = &rest[..close];
            if link.starts_with("http") || link.starts_with('#') {
                continue;
            }
            let file = link.split('#').next().unwrap_or(link);
            if !file.ends_with(".md") {
                continue;
            }
            let directory = page.parent().expect("a page lives in a directory");
            let path = directory.join(file);
            assert!(
                path.is_file(),
                "docs/sites/{site}.md links {link}, which does not exist next to it"
            );
        }
    }
}

/// The feature table and the reader table are the same list. A site behind a
/// feature that is not in the default set would disappear from an ordinary
/// build, which is not what "all seven by default" means.
#[test]
fn every_reader_is_a_feature_and_every_feature_is_a_reader() {
    let manifest = read("Cargo.toml");
    let default_line = manifest
        .lines()
        .find(|line| line.starts_with("default = ["))
        .expect("Cargo.toml has a default feature set");
    let feature_table = manifest
        .split("[features]")
        .nth(1)
        .expect("Cargo.toml has a [features] table");
    for site in tafas::sites::names() {
        assert!(
            default_line.contains(&format!("\"{site}\"")),
            "{site} is a reader but is not in the default feature set"
        );
        assert!(
            feature_table.contains(&format!("\n{site} = []")),
            "{site} is a reader but Cargo.toml has no `{site}` feature"
        );
    }
}

/// The crate that goes to crates.io ships the source and nothing else, and the
/// workflow that publishes it only uploads on a tag that is already released.
///
/// Both are promises a reader depends on and neither can be checked by shipping
/// a release: a version on crates.io cannot be replaced, and an archive that
/// carried `packaging/` or the fuzz workspace would be bytes nobody asked for,
/// forever. So they are checked here, where a mistake costs a red test.
#[test]
fn the_published_crate_is_the_source_and_publishes_only_on_a_tag() {
    let manifest = read("Cargo.toml");
    let include = manifest
        .split("include = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("Cargo.toml says what the published crate holds");
    for required in ["src/**", "Cargo.toml", "LICENSE", "README.md", "CHANGELOG.md"] {
        assert!(
            include.contains(&format!("\"{required}\"")),
            "the package must include {required}, which it does not"
        );
    }
    // The crates.io categories and keywords are capped (5 and 5), and a
    // validation failure there is an upload rejected after the version number is
    // already burned.
    let keywords = manifest
        .split("keywords = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("Cargo.toml lists keywords");
    assert!(
        keywords.split(',').count() <= 5,
        "crates.io accepts at most five keywords: {keywords}"
    );
    let categories = manifest
        .split("categories = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("Cargo.toml lists categories");
    assert!(
        categories.split(',').count() <= 5,
        "crates.io accepts at most five categories: {categories}"
    );

    let publish = read(".github/workflows/publish.yml");
    // The irreversible step is behind one condition, and that condition has to
    // mention the event and the token: a `workflow_dispatch` that published by
    // default would be a version number spent by a misclick.
    assert!(
        publish.contains("cargo publish"),
        "publish.yml must be the thing that publishes"
    );
    assert!(
        publish.contains("github.event_name") && publish.contains("CARGO_REGISTRY_TOKEN"),
        "the upload must be conditional on the event and on the token being present"
    );
    assert!(
        publish.contains("cargo package --list"),
        "publish.yml must check what the package holds before uploading it"
    );
    assert!(
        publish.contains("dry_run") || publish.contains("dry-run"),
        "a manual run must be able to package without publishing"
    );
}

/// Every script the release job runs has to exist, and the page that describes
/// the process has to name the script that is actually there.
///
/// This test exists because two of the release's steps were green while doing
/// nothing: `linux-packages.sh` skips itself when `nfpm` is missing, and the
/// `.apk` step ran a command that does not exist in the image it used. Both
/// produced a release with missing artifacts and a passing job, and both were
/// only found by cutting a tag. A path in the workflow is a reference like any
/// other, so it is checked like one.
#[test]
fn every_script_the_release_runs_exists() {
    let release = read(".github/workflows/release.yml");
    let docs = read("docs/releases.md");

    // `packaging/<name>.<sh|py>` in the workflow is a file that must be in the
    // repository; the workflow cannot discover a missing one until it runs.
    let mut seen = BTreeSet::new();
    for word in release.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '\\') {
        let word = word.trim_end_matches([',', ';', ')']);
        if word.starts_with("packaging/") && (word.ends_with(".sh") || word.ends_with(".py")) {
            assert!(
                manifest_dir().join(word).is_file(),
                "release.yml runs {word}, which does not exist"
            );
            seen.insert(word.to_string());
        }
    }
    assert!(
        seen.contains("packaging/apk.py"),
        "the release must build the Alpine package; it is an asset like the .deb: {seen:?}"
    );

    // A download URL that is written out in full is a reference too, and the
    // one for `nfpm` was to a name that has never existed: its assets carry the
    // version (`nfpm_2.48.0_amd64.deb`), so `releases/latest/download/nfpm_amd64.deb`
    // answered 404. Nothing noticed, because the script it fed skips itself when
    // the tool is missing — a green step and no `.deb` or `.rpm` in the release.
    assert!(
        !release.contains("latest/download/nfpm_"),
        "release.yml downloads nfpm by a name without its version, which 404s"
    );
    assert!(
        release.contains("nfpm_${version}_amd64.deb"),
        "release.yml must build the nfpm asset name from the version it asked for"
    );

    // The prose names the packers too, and it was wrong for a release: it said
    // `apk.sh` and `apk mkpkg` after both had gone.
    // `apk mkpkg` is allowed in one place: the sentence that explains why the
    // format was written here instead. "apk.sh" as a file name is not.
    assert!(
        !docs.contains("apk.sh"),
        "docs/releases.md still names packaging/apk.sh, which no longer exists"
    );
    assert!(
        !docs.contains("alpine:3.20"),
        "docs/releases.md still says the .apk is built in a container"
    );
    assert!(
        docs.contains("packaging/apk.py"),
        "docs/releases.md must say which script builds the .apk"
    );
}
