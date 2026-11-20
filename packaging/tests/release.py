#!/usr/bin/env python3
"""Check draft publication ordering without making GitHub calls."""

import hashlib
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from release import publish, required_assets

TAG = "v0.4.2"


class ReleaseTest(unittest.TestCase):
    def exercise(self, directory, *, draft, corrupt=False):
        events = []

        def fake_gh(*args):
            command = args[1]
            if command == "list":
                return "" if draft else "false"
            if command == "create":
                self.assertIn("--draft", args)
            if command == "view":
                return "" if "assets" in args else "https://example.test/release"
            if command == "download":
                assets = Path(args[4])
                assets.mkdir()
                sums = []
                for name in required_assets(TAG):
                    payload = name.encode()
                    sums.append(f"{hashlib.sha256(payload).hexdigest()}  {name}")
                    (assets / name).write_bytes(payload)
                (assets / "SHA256SUMS").write_text("\n".join(sums) + "\n")
                if corrupt:
                    (assets / "install.ps1").write_bytes(b"changed")
            events.append(command)
            return ""

        with patch("release.gh", side_effect=fake_gh), patch(
            "release.build", side_effect=lambda tag, base: events.append("build"),
        ):
            if corrupt:
                with self.assertRaisesRegex(ValueError, "SHA-256"):
                    publish(TAG, "main", "Notes", directory)
            else:
                self.assertIn('version "0.4.2"', publish(TAG, "main", "Notes", directory))
        return events

    def test_builds_and_verifies_the_draft_before_publishing(self):
        with TemporaryDirectory() as directory:
            self.assertEqual(
                self.exercise(Path(directory), draft=True)[:4],
                ["create", "build", "download", "edit"],
            )

    def test_bad_checksums_leave_the_release_in_draft(self):
        with TemporaryDirectory() as directory:
            self.assertEqual(
                self.exercise(Path(directory), draft=True, corrupt=True),
                ["create", "build", "download"],
            )

    def test_a_published_release_can_resume_at_the_formula(self):
        with TemporaryDirectory() as directory:
            self.assertEqual(self.exercise(Path(directory), draft=False), ["download"])


if __name__ == "__main__":
    unittest.main()
