#!/usr/bin/env python3
"""Build, verify and publish a GitHub release, then open the Homebrew PR."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from tempfile import TemporaryDirectory

from channels import DEPENDS, homebrew_formula, read_sums, sha256_file

ROOT = Path(__file__).resolve().parent.parent
REPO = "RUdolf1517/tafas"
FORMULA = Path("Formula/tafas.rb")


def run(*command, cwd=ROOT, capture=True):
    return subprocess.run(
        command, cwd=cwd, check=True, text=True,
        stdout=subprocess.PIPE if capture else None,
    ).stdout


def gh(*args, **kwargs):
    output = run("gh", *args, "--repo", REPO, **kwargs)
    return output.strip() if output is not None else None


def git(*args, **kwargs):
    return run("git", *args, **kwargs).strip()


def notes_at(commit, tag):
    manifest = git("show", f"{commit}:Cargo.toml")
    if not re.search(rf'^version = "{re.escape(tag[1:])}"$', manifest, re.MULTILINE):
        raise ValueError(f"{commit} does not set Cargo.toml to {tag[1:]}")
    changelog = git("show", f"{commit}:CHANGELOG.md")
    section = re.search(
        rf"^## \[{re.escape(tag[1:])}\][^\n]*\n(.*?)(?=^## |\Z)",
        changelog, re.MULTILINE | re.DOTALL,
    )
    if not section or not section[1].strip():
        raise ValueError(f"CHANGELOG.md has no notes for {tag}")
    return section[1].strip()


def required_assets(tag):
    version = tag[1:]
    return [
        *DEPENDS, "install.ps1", "tafas_amd64.deb", "tafas_arm64.deb",
        "tafas.x86_64.rpm", "tafas.aarch64.rpm",
        f"tafas-{version}-aarch64.apk", f"tafas-{version}-x86_64.apk",
        f"tafas-channels-{version}.tar.gz", f"tafas-release-{version}.spdx.json",
        f"tafas-winget-local-manifests-{tag}.zip",
    ]


def verify_assets(directory, tag):
    if not (directory / "SHA256SUMS").is_file():
        raise ValueError(f"{tag} has no SHA256SUMS")
    sums = read_sums(directory)
    expected = required_assets(tag)
    for name in expected:
        if not (directory / name).is_file() or name not in sums:
            raise ValueError(f"{tag} is missing {name} or its checksum")
    for name, digest in sums.items():
        asset = directory / name
        if not asset.is_file() or sha256_file(asset) != digest:
            raise ValueError(f"{tag} is missing {name} or its SHA-256 disagrees")
    return homebrew_formula(tag[1:], sums)


def build(tag, base):
    runs = json.loads(gh(
        "run", "list", "--workflow", "release.yml", "--branch", base,
        "--event", "workflow_dispatch", "--limit", "30",
        "--json", "databaseId,displayTitle,status",
    ))
    pending = next((r for r in runs if r["displayTitle"] == f"Build {tag}"
                    and r["status"] != "completed"), None)
    if pending:
        run_id = pending["databaseId"]
    else:
        result = json.loads(run(
            "gh", "api", "--method", "POST",
            f"repos/{REPO}/actions/workflows/release.yml/dispatches",
            "-H", "X-GitHub-Api-Version: 2026-03-10", "-f", f"ref={base}", "-f", f"inputs[tag]={tag}",
            "-F", "return_run_details=true",
        ))
        run_id = result["workflow_run_id"]
    gh("run", "watch", str(run_id), "--exit-status", "--interval", "15", capture=False)


def formula_pr(tag, base, formula, worktree):
    git("fetch", "origin", base)
    base_commit = git("rev-parse", "FETCH_HEAD")
    notes_at(base_commit, tag)
    if (git("show", f"{base_commit}:{FORMULA}") + "\n") == formula:
        print(f"{FORMULA} already describes {tag} on {base}")
        return
    branch = f"release/formula-{tag}"
    existing = gh("pr", "list", "--base", base, "--head", branch,
                  "--json", "url", "--jq", ".[0].url // empty")
    if existing:
        print(existing)
        return
    if git("ls-remote", "origin", f"refs/heads/{branch}"):
        git("fetch", "origin", branch)
        if ((git("show", f"FETCH_HEAD:{FORMULA}") + "\n") != formula
                or git("diff", "--name-only", f"{base_commit}...FETCH_HEAD") != str(FORMULA)):
            raise ValueError(f"existing branch {branch} differs from the generated formula")
    else:
        git("worktree", "add", "--detach", str(worktree), base_commit)
        try:
            (worktree / FORMULA).write_text(formula, encoding="utf-8")
            git("add", str(FORMULA), cwd=worktree)
            git("commit", "-m", f"Update Homebrew formula for {tag}", cwd=worktree)
            git("push", "origin", f"HEAD:refs/heads/{branch}", cwd=worktree)
        finally:
            git("worktree", "remove", str(worktree))
    print(gh(
        "pr", "create", "--base", base, "--head", branch,
        "--title", f"Update Homebrew formula for {tag}",
        "--body", f"Generated from the verified SHA256SUMS published with {tag}.",
    ))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag", help="a version already committed to the default branch, such as v0.4.2")
    args = parser.parse_args()
    tag = args.tag
    if not re.fullmatch(r"v\d+\.\d+\.\d+", tag, re.ASCII):
        parser.error("expected a tag such as v0.4.2")

    # Use the operator's stored gh session rather than an Actions token.
    for key in ("GH_TOKEN", "GITHUB_TOKEN"):
        os.environ.pop(key, None)
    user = json.loads(run("gh", "api", "--hostname", "github.com", "user"))
    if user["type"] != "User":
        raise ValueError("sign in to gh with a personal GitHub account")
    origin = git("remote", "get-url", "origin")
    if not re.fullmatch(r"(?:https://github\.com/|git@github\.com:)"
                        + re.escape(REPO) + r"(?:\.git)?/?", origin, re.IGNORECASE):
        raise ValueError(f"origin must point to {REPO}")
    base = run("gh", "repo", "view", REPO, "--json", "defaultBranchRef",
               "--jq", ".defaultBranchRef.name").strip()
    git("fetch", "origin", base)
    base_commit = git("rev-parse", "FETCH_HEAD")
    notes = notes_at(base_commit, tag)
    ref = f"refs/tags/{tag}"
    tagged = git("ls-remote", "origin", ref)
    if tagged:
        git("fetch", "--no-tags", "origin", ref)
        commit = git("rev-parse", "FETCH_HEAD^{commit}")
        git("merge-base", "--is-ancestor", commit, base_commit)
        notes = notes_at(commit, tag)
    else:
        git("push", "origin", f"{base_commit}:{ref}")

    with TemporaryDirectory(prefix="tafas-release-") as tmp:
        directory = Path(tmp)
        formula = publish(tag, base, notes, directory)
        formula_pr(tag, base, formula, directory / "worktree")


def publish(tag, base, notes, directory):
    release = gh("release", "list", "--limit", "100", "--json", "tagName,isDraft",
                 "--jq", f'.[] | select(.tagName == "{tag}") | .isDraft')
    if not release:
        notes_file = directory / "notes.md"
        notes_file.write_text(notes + "\n", encoding="utf-8")
        gh("release", "create", tag, "--draft", "--verify-tag",
           "--title", f"tafas {tag[1:]}", "--notes-file", str(notes_file))
        release = "true"
    assets = gh("release", "view", tag, "--json", "assets", "--jq", ".assets[].name")
    built = release == "true" and "SHA256SUMS" not in assets.splitlines()
    if built:
        build(tag, base)
    downloads = directory / "assets"
    gh("release", "download", tag, "--dir", str(downloads))
    try:
        formula = verify_assets(downloads, tag)
    except ValueError:
        if release != "true" or built:
            raise
        # An interrupted asset upload can leave the checksum file behind.
        build(tag, base)
        downloads = directory / "rebuilt-assets"
        gh("release", "download", tag, "--dir", str(downloads))
        formula = verify_assets(downloads, tag)
    if release == "true":
        gh("release", "edit", tag, "--draft=false", "--latest")
    print(gh("release", "view", tag, "--json", "url", "--jq", ".url"))
    return formula


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(f"release: {error}")
