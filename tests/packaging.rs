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
        .expect("Cargo.toml has a [features] table")
        .split("\n[")
        .next()
        .unwrap_or_default();
    for site in tafas::sites::names() {
        assert!(
            default_line.contains(&format!("\"{site}\"")),
            "{site} is a reader but is not in the default feature set"
        );
        assert!(
            feature_table.lines().any(|line| {
                line.split_once('=')
                    .is_some_and(|(name, _)| name.trim() == site)
            }),
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
    for required in [
        "src/**",
        "Cargo.toml",
        "LICENSE",
        "README.md",
        "CHANGELOG.md",
    ] {
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

    // The script the owner runs on their own machine is the same procedure, and
    // a check that lives only in a workflow is a check that does nothing on the
    // path this project's own documentation recommends first.
    let script = read("packaging/publish.sh");
    for check in [
        "cargo package --list",
        "grep -qx",
        "cargo publish",
        "--check",
        "git status --porcelain",
    ] {
        assert!(
            script.contains(check),
            "packaging/publish.sh no longer does {check}"
        );
    }
    assert!(
        !script.contains("cargo publish --allow-dirty"),
        "publishing a dirty tree is publishing a tree nobody can check out"
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
    // Publishing from a machine and then from the release tag would upload the
    // same version twice, and the second one is a rejection on a release that
    // went perfectly. The workflow has to look first.
    assert!(
        publish.contains("crates.io/api/v1/crates/tafas/"),
        "publish.yml must ask crates.io whether the version is already published"
    );
}

/// The number on the crate and the notes inside it are the same promise, so both
/// paths that spend a number check that the changelog has a section for it.
///
/// This is the mistake the release process came closest to: `Cargo.toml` said
/// 0.2.1 and `CHANGELOG.md` had `## [Unreleased]`, because the version had been
/// bumped on one commit and the work described under a heading belonged to
/// another. The archive carries `CHANGELOG.md`, so the error would have shipped.
#[test]
fn the_changelog_describes_the_version_being_published() {
    let version = env!("CARGO_PKG_VERSION");
    let changelog = read("CHANGELOG.md");
    assert!(
        changelog.contains(&format!("## [{version}]")),
        "CHANGELOG.md has no `## [{version}]` section, so the archive would ship \
         notes that never mention the version they are for"
    );

    // And both paths that upload check it themselves, so the test is not the only
    // thing standing between a stale changelog and a published version.
    let script = read("packaging/publish.sh");
    assert!(
        script.contains("CHANGELOG.md has a section"),
        "packaging/publish.sh must refuse a version the changelog does not describe"
    );
    let publish = read(".github/workflows/publish.yml");
    assert!(
        publish.contains("CHANGELOG.md has a section for this version"),
        "publish.yml must refuse a version the changelog does not describe"
    );
}

/// The Homebrew formula is generated from the release's `SHA256SUMS` like every
/// other channel, and this is the check that it was.
///
/// It used to be the one hand edit in the release process, and 0.3.0 was tagged
/// with four zeroes in it: `brew install` downloaded four megabytes and then
/// refused the archive on its checksum. The fix is not a better reminder — it is
/// that `packaging/channels.py` now renders this file the way it renders the
/// `PKGBUILD`, the Scoop manifest and `flake.nix`, so there is nothing left to
/// paste.
///
/// The formula must match the generator for the version it installs. It can
/// stay on the previous release while the next version is being packaged.
#[test]
fn the_homebrew_formula_names_hashes_the_release_published() {
    let formula = read("Formula/tafas.rb");
    let version = formula
        .lines()
        .find_map(|line| line.strip_prefix("  version \"")?.strip_suffix('"'))
        .expect("Formula/tafas.rb has a version");
    let parse = |version: &str| {
        let components: Vec<u64> = version
            .split('.')
            .map(|part| part.parse().expect("a numeric version component"))
            .collect();
        assert_eq!(components.len(), 3, "expected a release version: {version}");
        components
    };
    assert!(
        parse(version) <= parse(env!("CARGO_PKG_VERSION")),
        "Formula/tafas.rb points to a version newer than the crate"
    );

    // A zeroed hash is a placeholder, and a placeholder is an install that fails
    // with a message about a checksum rather than about an edit somebody forgot
    // to make. The one state in which it is allowed is a version that has not
    // been built yet, and the file has to say so on the line the generator
    // writes — a declaration, not a comment somebody added to get a green run.
    //
    // That declaration is only believed here because `packaging/tests/formula.py`
    // is the check that verifies it against GitHub, in CI, on every pull request.
    let zeroes = "0".repeat(64);
    let placeholders = formula.contains(&format!("sha256 \"{zeroes}\""));
    let declared = format!("# unpublished: v{version} has not been built");
    if placeholders {
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        assert!(
            formula.contains(&declared),
            "Formula/tafas.rb carries a zero-filled sha256 and does not declare \
             `{declared}`: either the release exists and the hashes are missing, or the \
             explanation was edited out"
        );
    }
    // The other direction: a formula with no zeros has nothing to declare, and a
    // stale declaration would be a lie about a version that is already out.
    if !placeholders {
        assert!(
            !formula.contains("# unpublished:"),
            "Formula/tafas.rb declares an unpublished version but carries real hashes: \
             the declaration is stale and should have been removed by a regeneration"
        );
    }

    let hashes: Vec<&str> = formula
        .match_indices("sha256 \"")
        .map(|(at, marker)| {
            let rest = &formula[at + marker.len()..];
            &rest[..rest.find('"').expect("an unclosed sha256 string")]
        })
        .collect();
    assert_eq!(
        hashes.len(),
        4,
        "the formula installs macOS and Linux for two architectures: {hashes:?}"
    );
    for hash in &hashes {
        assert_eq!(hash.len(), 64, "{hash} is not a sha256");
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{hash} is not lowercase hex"
        );
    }
    // Four different archives have four different hashes; two equal values mean
    // one was pasted twice. Four *placeholders* are equal by definition, and the
    // declaration above is what says so — a real hash repeated twice is still the
    // mistake this catches.
    if !placeholders {
        let mut unique = hashes.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 4, "the formula repeats a hash: {hashes:?}");
    }

    // The formula is what the generator writes. The four hashes cannot be known
    // before the release exists, so this renders against a stand-in `SHA256SUMS`
    // and compares everything else: if the two disagree on anything the generator
    // produces — a block, a caveat, the test stanza — the file was edited by hand
    // and `brew` will read something the release job never wrote.
    // The header is allowed to carry a note explaining the current state of the
    // hashes; everything the formula *is* starts at `class`. So the comparison
    // begins there and runs to the end, with the hash lines removed — they are
    // the one thing that differs between a rendering and the published release.
    let body = |text: &str| -> Vec<String> {
        text.lines()
            .skip_while(|line| !line.starts_with("class "))
            .filter(|line| !line.contains("sha256 \""))
            .map(|line| line.to_string())
            .collect()
    };
    let generated = render_formula(version);
    assert!(
        formula.contains("Generated by packaging/channels.py"),
        "Formula/tafas.rb does not say it is generated, so the next person will edit it by hand"
    );
    assert_eq!(
        body(&formula),
        body(&generated),
        "Formula/tafas.rb is not what packaging/channels.py renders: edit the generator, \
         then regenerate (docs/releases.md §4 step 5)"
    );

    // And the check itself runs, on both the path CI takes and the path a person
    // takes, and it is wired into the workflow that runs on every pull request.
    let checker = read("packaging/tests/formula.py");
    assert!(
        checker.contains("--sums") && checker.contains("--release"),
        "packaging/tests/formula.py must work offline (--sums) and against a release (--release)"
    );
    let ci = read(".github/workflows/ci.yml");
    assert!(
        ci.contains("packaging/tests/formula.py"),
        "CI must compare the formula with the release, or the next release's zeroes ship too"
    );
}

/// The licence claim in the readme is checked by a job that exists.
///
/// `deny.toml` sat in this repository for three releases while three pages said
/// `cargo deny` ran "when a release is cut" and no workflow mentioned it. A
/// configuration for a command nobody runs is a claim nobody checks, and the
/// claim here is the licence on the front page.
#[test]
fn the_licence_check_runs_and_says_what_it_is() {
    let ci = read(".github/workflows/ci.yml");
    assert!(
        ci.contains("cargo deny"),
        "CI must run `cargo deny`: deny.toml is otherwise a policy nobody applies"
    );
    assert!(
        ci.contains("--all-features"),
        "the licence check must cover the feature builds a reader is told to make"
    );
    // And the sentence that made this test necessary: the one that said the
    // check ran at release time and nowhere else.
    for page in ["README.md", "docs/dependencies.md", "docs/testing.md"] {
        let text = read(page).split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            !text.contains("when a release is cut;"),
            "{page} still says the licence check happens at release time"
        );
    }
    let contributing = read("CONTRIBUTING.md");
    assert!(
        contributing.contains("cargo deny check"),
        "CONTRIBUTING.md must name the command a contributor runs by hand"
    );
}

/// The allow-list in `deny.toml` and the allow-list quoted in the prose are the
/// same list, and every entry in it is an SPDX identifier `cargo deny` accepts.
///
/// This test exists because the check's first real run failed on a licence that
/// was in neither place: `webpki-roots` ships the Mozilla certificate store
/// under `CDLA-Permissive-2.0`, which is data rather than code and so had been
/// missed by a list written from the crates this project chooses. A page that
/// quotes the list is how the next reader knows what will pass.
#[test]
fn the_allow_list_is_one_list() {
    let deny = read("deny.toml");
    let listed: Vec<String> = deny
        .split_once("allow = [")
        .expect("deny.toml has a licence allow-list")
        .1
        .split_once(']')
        .expect("the allow-list is closed")
        .0
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.trim_end_matches(',').trim_matches('"').to_string())
        .collect();
    assert!(
        listed.contains(&"CDLA-Permissive-2.0".to_string()),
        "the certificate bundle's licence has to be allowed, or the check fails"
    );
    let page = read("docs/dependencies.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for licence in &listed {
        assert!(
            page.contains(licence.as_str()),
            "docs/dependencies.md quotes an allow-list that has lost `{licence}`"
        );
    }
}

/// The `licenses` job reports through a pull-request comment, because the job
/// log lives behind a URL that expires. That comment is therefore the whole
/// failure, and the first one this job ever produced put eighty lines of
/// warnings in it and cut the error that caused the failure.
#[test]
fn the_licence_failure_is_reported_from_the_error() {
    let ci = read(".github/workflows/ci.yml");
    assert!(
        !ci.contains("tail -n 80 deny.log"),
        "reporting the tail of the log hides the error behind the warnings"
    );
    assert!(
        ci.contains("/^error/ {keep=1} keep"),
        "the published failure must start at the first error"
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
/// `cargo audit` is not per-commit, and the reason is the database rather than
/// the code: RustSec moves while this repository does not. The arrangement is
/// therefore a schedule and a tag, and it has to be an arrangement that *says*
/// something — a job that fails every Monday in silence is a job nobody has.
///
/// This test exists in the same spirit as the one above it: the previous version
/// of this project said the licence check ran "when a release is cut" and it ran
/// nowhere, so what a page claims about a workflow is now checked against the
/// workflow.
#[test]
fn the_advisory_check_runs_on_a_clock_and_says_so() {
    let audit = read(".github/workflows/audit.yml");
    assert!(
        audit.contains("schedule:"),
        "`cargo audit` has to run without anybody touching the repository"
    );
    assert!(
        audit.contains("cargo audit"),
        "audit.yml must actually invoke cargo audit"
    );
    assert!(
        audit.contains("tags:"),
        "a tag is the last moment before archives go out; audit there too"
    );
    // A scheduled run has no pull request to comment on, so silence is the
    // failure mode to design against.
    assert!(
        audit.contains("gh issue create") && audit.contains("gh issue close"),
        "a scheduled failure must open an issue and close it when clean"
    );

    // And the pages that describe the cadence have to describe this one. The
    // sentence they used to carry — that it is "not wired into a workflow" — was
    // true until this job existed and is now the exact kind of claim this test
    // is for.
    for page in [
        "docs/dependencies.md",
        "docs/testing.md",
        "CONTRIBUTING.md",
        "docs/releases.md",
    ] {
        let text = read(page).split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            text.contains("audit.yml"),
            "{page} does not name the workflow that runs `cargo audit`"
        );
        assert!(
            !text.contains("not wired into a workflow"),
            "{page} still says the advisory check runs nowhere"
        );
    }
}

/// Runs the real generator against a stand-in release and returns the formula.
///
/// The generator is the only thing that knows what the formula should say, so a
/// test that reimplemented it would be a second source of truth — exactly the
/// problem the generator exists to remove. `python3` is already a dependency of
/// the release job and of `--self-test`, which CI runs on every pull request.
fn render_formula(version: &str) -> String {
    use std::process::Command;

    let dir = std::env::temp_dir().join(format!("tafas-formula-{}", std::process::id()));
    let artifacts = dir.join("artifacts");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&artifacts).expect("create the stand-in release");
    let mut sums = String::new();
    for name in [
        "tafas-x86_64-unknown-linux-musl.tar.gz",
        "tafas-aarch64-unknown-linux-musl.tar.gz",
        "tafas-x86_64-apple-darwin.tar.gz",
        "tafas-aarch64-apple-darwin.tar.gz",
        "tafas-x86_64-pc-windows-msvc.zip",
    ] {
        std::fs::write(artifacts.join(name), name).expect("write a stand-in asset");
        // The generator hashes the assets it finds and refuses a `SHA256SUMS`
        // that disagrees with them, so the file is written to match.
        let digest = sha256(artifacts.join(name));
        sums.push_str(&format!("{digest}  ./{name}\n"));
    }
    std::fs::write(artifacts.join("SHA256SUMS"), sums).expect("write the stand-in sums");

    let output = Command::new("python3")
        .arg(manifest_dir().join("packaging/channels.py"))
        .arg("--tag")
        .arg(format!("v{version}"))
        .arg("--artifacts")
        .arg(&artifacts)
        .arg("--output-dir")
        .arg(dir.join("out"))
        .output()
        .expect("python3 runs packaging/channels.py");
    assert!(
        output.status.success(),
        "packaging/channels.py failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // `--output-dir` strips the `packaging/` prefix, so the three channel files
    // land at the top of that directory and the formula keeps its own path.
    //
    // Read before the directory is removed: this used to delete `dir` first and
    // then read from it, which reported "the generator wrote a formula" on a
    // miss — a description of what was wanted, not of what happened.
    //
    // `read_to_string` and not `read_to_end`: the generator writes UTF-8, and a
    // formula read back as invalid UTF-8 means it was written in the machine's
    // ANSI code page. That is a real failure on Windows, where the default text
    // encoding is not UTF-8, and the header's section sign is outside ASCII.
    let rendered = std::fs::read_to_string(dir.join("out/Formula/tafas.rb"))
        .expect("the generator wrote a formula at out/Formula/tafas.rb, in UTF-8");
    let _ = std::fs::remove_dir_all(&dir);
    rendered
}

/// A file's SHA-256, in the hex form `SHA256SUMS` uses.
fn sha256(path: std::path::PathBuf) -> String {
    use std::io::Read;

    let mut file = std::fs::File::open(&path).expect("open the stand-in asset");
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .expect("read the stand-in asset");
    sha256_hex(&bytes)
}

/// SHA-256 in plain Rust: the test target has no `sha2` dependency, and pulling
/// one in for a fixture would be a dependency the release graph does not need.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}
