#!/usr/bin/env python3
"""Download and inspect one public example per site."""
import argparse
import json
import subprocess
import tempfile
from pathlib import Path


CASES = {
    "youtube": ("https://www.youtube.com/watch?v=aqz-KE-bpKQ", "video", ()),
    "tiktok": ("https://www.tiktok.com/@scout2015/video/6718335390845095173", "video", ()),
    "x": ("https://x.com/historyinmemes/status/1790637656616943991", "video", ()),
    "vimeo": ("https://vimeo.com/22439234", "video", ("--playlist-items", "1")),
    "reddit": ("https://v.redd.it/a2mdj5d57qyc1", "video", ("--playlist-items", "1")),
    "reddit-embed": ("https://www.reddit.com/r/Unexpected/comments/1cl9h0u/", "video", ("--playlist-items", "1")),
    "soundcloud": ("https://soundcloud.com/the80m/the-following", "audio", ("--playlist-items", "1")),
    "twitch": ("https://clips.twitch.tv/FaintLightGullWholeWheat", "video", ()),
}


def capture(command):
    result = subprocess.run(command, capture_output=True, text=True, timeout=95)
    try:
        item = json.loads(result.stdout.splitlines()[-1])
    except (IndexError, ValueError):
        item = {}
    return result.returncode, item


def probe(binary, site, cookies):
    url, expected, flags = CASES[site]
    command = [binary, "--json", "--retries", "0", "--timeout", "10"]
    if site == "youtube" and cookies:
        command += ["--cookies", str(cookies)]
    code, item = capture([*command, "--dry-run", url])
    if code:
        return f"extraction or selection failed ({item.get('reason') or code})"

    with tempfile.TemporaryDirectory(prefix="tafas-smoke-") as directory:
        code, item = capture([
            *command, "-Q", "worst", "--max-filesize", "30M", "-o", directory, *flags, url,
        ])
        if code:
            return f"download or assembly failed ({item.get('reason') or code})"

        for path in Path(directory).iterdir():
            if path.suffix not in {".mp4", ".m4a", ".mp3", ".webm"}:
                continue
            report = subprocess.run([
                "ffprobe", "-v", "error", "-show_entries", "stream=codec_type,nb_read_packets",
                "-count_packets", "-of", "json", str(path),
            ], capture_output=True, text=True, timeout=15)
            if report.returncode:
                continue
            verified = {
                stream["codec_type"]
                for stream in json.loads(report.stdout)["streams"]
                if str(stream.get("nb_read_packets", "")).isdigit()
                and int(stream["nb_read_packets"]) > 0
            }
            if expected in verified:
                return None
    return f"verification failed (no readable {expected} packets)"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", default="target/debug/tafas")
    parser.add_argument("--youtube-cookies", type=Path)
    parser.add_argument("sites", nargs="*", metavar="SITE", help=", ".join(CASES))
    args = parser.parse_args()
    sites = args.sites or CASES
    if unknown := set(sites) - CASES.keys():
        parser.error("unknown sites: " + ", ".join(sorted(unknown)))
    failed = False
    if args.youtube_cookies and not args.youtube_cookies.is_file():
        parser.error("--youtube-cookies must name a local file")
    for site in sites:
        try:
            error = probe(args.bin, site, args.youtube_cookies)
        except subprocess.TimeoutExpired:
            error = "timed out"
        print(f"{site}: {error or 'ok'}", flush=True)
        failed |= error is not None
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
