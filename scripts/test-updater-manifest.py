#!/usr/bin/env python3
import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = pathlib.Path(__file__).with_name("updater-manifest.py")
SPEC = importlib.util.spec_from_file_location("updater_manifest", SCRIPT)
updater = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(updater)


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = pathlib.Path(self.temp.name)
        for platform in ["macos-aarch64", "macos-x86_64", "linux-aarch64", "linux-x86_64"]:
            for suffix in (["app.tar.gz"] if platform.startswith("macos") else ["AppImage", "deb"]):
                name = f"init-desktop-v0.6.0-{platform}.{suffix}"
                (self.directory / name).write_bytes(b"signed package fixture")
                (self.directory / f"{name}.sig").write_text(f"signature for {name}\n")

    def test_feed_carries_exact_signed_payload_for_each_installer(self):
        result = updater.manifest(self.directory, "v0.6.0", "djsydney04/init")
        self.assertEqual(result["version"], "0.6.0")
        expected = {
            "darwin-aarch64": "macos-aarch64.app.tar.gz",
            "darwin-x86_64": "macos-x86_64.app.tar.gz",
            "linux-aarch64-appimage": "linux-aarch64.AppImage",
            "linux-x86_64-appimage": "linux-x86_64.AppImage",
            "linux-aarch64-deb": "linux-aarch64.deb",
            "linux-x86_64-deb": "linux-x86_64.deb",
        }
        self.assertEqual(set(result["platforms"]), set(expected))
        for target, suffix in expected.items():
            name = f"init-desktop-v0.6.0-{suffix}"
            self.assertEqual(result["platforms"][target], {
                "url": f"https://github.com/djsydney04/init/releases/download/v0.6.0/{name}",
                "signature": f"signature for {name}",
            })

    def test_incomplete_release_cannot_publish(self):
        signature = self.directory / "init-desktop-v0.6.0-linux-x86_64.deb.sig"
        signature.write_text("\n")
        with self.assertRaisesRegex(ValueError, "Missing signature"):
            updater.manifest(self.directory, "v0.6.0", "djsydney04/init")
        signature.unlink()
        with self.assertRaises(FileNotFoundError):
            updater.manifest(self.directory, "v0.6.0", "djsydney04/init")

    def test_older_rebuild_does_not_move_channel_backwards(self):
        previous = self.directory / "previous.json"
        previous.write_text(json.dumps({"version": "0.10.0"}))
        output = self.directory / "github-output"
        subprocess.run([
            sys.executable, str(SCRIPT), str(self.directory), "v0.6.0", "djsydney04/init",
            "--previous", str(previous), "--github-output", str(output),
        ], check=True, capture_output=True)
        self.assertEqual(output.read_text(), "publish=false\n")
        self.assertEqual(json.loads((self.directory / "latest.json").read_text())["version"], "0.6.0")
        self.assertTrue(updater.should_publish({"version": "0.10.0"}, {"version": "0.9.0"}))
        self.assertTrue(updater.should_publish({"version": "0.6.0"}, {"version": "0.6.0"}))

    def test_invalid_version_and_repo_are_rejected(self):
        for value in ["../v1.0.0", "v01.0.0", "v0.6", "v0.6.0-rc.1"]:
            with self.assertRaises(ValueError):
                updater.manifest(self.directory, value, "djsydney04/init")
        with self.assertRaises(ValueError):
            updater.manifest(self.directory, "v0.6.0", "djsydney04/init/../other")

    def test_channel_version_survives_a_missing_manifest(self):
        response = 'HTTP/2.0 200 OK\nContent-Type: application/json\n\n' + json.dumps({
            "draft": False, "body": '{"version":"0.10.0"}', "assets": [],
        })
        with patch.object(updater.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, response, "")):
            self.assertEqual(updater.channel_version("djsydney04/init", "desktop-updates"), "0.10.0")

    def test_only_absent_or_unpublished_channels_bypass_version_ordering(self):
        cases = [
            (404, 1, '{"message":"Not Found"}', None),
            (200, 0, '{"draft":true,"body":"initial channel"}', None),
        ]
        for status, code, body, expected in cases:
            response = f"HTTP/2.0 {status} Response\n\n{body}"
            with patch.object(updater.subprocess, "run", return_value=subprocess.CompletedProcess([], code, response, "")):
                self.assertEqual(updater.channel_version("djsydney04/init", "desktop-updates"), expected)
        for response in ["", "HTTP/2.0 500 Server Error\n\n{}", "HTTP/2.0 403 Forbidden\n\n{}"]:
            with patch.object(updater.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, response, "")):
                with self.assertRaises(RuntimeError):
                    updater.channel_version("djsydney04/init", "desktop-updates")


if __name__ == "__main__":
    unittest.main()
