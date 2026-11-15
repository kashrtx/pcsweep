import tempfile
import json
from pathlib import Path
import unittest
from unittest.mock import patch
import zipfile

import publish_release as release


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.dist = Path(self.folder.name)
        self.exe = self.dist / "PCSweep.exe"
        self.exe.write_bytes(b"MZtested-Windows-binary")
        with zipfile.ZipFile(self.dist / "PCSweep-1.1.0-windows-x64.zip", "w") as package:
            package.write(self.exe, self.exe.name)
            for name in ["README.md", "LICENSE", "RELEASE_NOTES.md"]:
                package.writestr(name, "Documentation")
        self.commit = "a" * 40
        self.created = []
        self.gh_calls = []
        self.remote_releases = []
        self.tag_exists = False
        self.race = False

    def api(self, route, payload=None, method="GET"):
        if method == "POST":
            if self.race and payload["tag_name"] == "v1.1.0":
                raise release.GitHubError("already_exists (HTTP 422)")
            item = dict(payload, id=7, html_url="https://github.com/test/app/releases/7")
            self.created.append(item)
            return item
        if method == "PATCH":
            return dict(self.created[-1], **payload)
        if route.endswith("heads/main"):
            return {"object": {"sha": self.commit}}
        raise AssertionError(route)

    def pages(self, route):
        if "/assets?" in route:
            return [
                {"name": path.name, "size": path.stat().st_size, "digest": f"sha256:{release.digest(path)}"}
                for path in self.dist.iterdir()
            ]
        return self.remote_releases

    def publish(self):
        with patch.object(release, "api", side_effect=self.api), patch.object(
            release, "pages", side_effect=self.pages
        ), patch.object(release, "existing_tag", return_value=self.tag_exists), patch.object(
            release, "gh", side_effect=lambda *args: self.gh_calls.append(args)
        ):
            return release.publish("1.1.0", self.dist, "test/app", self.commit, "42", "https://build", "Notes")

    def test_first_push_creates_base_release_with_all_assets(self):
        self.publish()
        self.assertEqual(self.created[0]["tag_name"], "v1.1.0")
        self.assertTrue(self.created[0]["draft"])
        self.assertEqual(self.created[0]["target_commitish"], self.commit)
        checksums = (self.dist / "SHA256SUMS.txt").read_text()
        self.assertIn(f"{release.digest(self.exe)}  PCSweep.exe", checksums)
        self.assertEqual(len(checksums.splitlines()), 3)
        manifest = json.loads((self.dist / "update.json").read_text())
        self.assertEqual(manifest, {
            "schema": 1, "version": "1.1.0", "revision": 42,
            "commit": self.commit, "arch": "windows-x64", "asset": "PCSweep.exe",
            "size": self.exe.stat().st_size, "sha256": release.digest(self.exe),
        })

    def test_unchanged_version_still_publishes_unique_build(self):
        self.tag_exists = True
        self.publish()
        self.assertEqual(self.created[0]["tag_name"], "v1.1.0+build.42")
        self.assertTrue((self.dist / "PCSweep-1.1.0+build.42-windows-x64.zip").is_file())

    def test_base_version_race_uses_unique_build(self):
        self.race = True
        self.publish()
        self.assertEqual(self.created[0]["tag_name"], "v1.1.0+build.42")

    def test_published_rerun_verifies_without_mutations(self):
        published = {"body": f"<!-- pcsweep-commit:{self.commit} -->", "draft": False, "html_url": "existing"}
        self.remote_releases = [published]
        with patch.object(release, "verify_published") as verify:
            self.assertEqual(self.publish(), "existing")
            verify.assert_called_once_with("test/app", published)
        self.assertFalse(self.created)
        self.assertFalse(self.gh_calls)

    def test_draft_rerun_resumes_upload_without_new_release(self):
        self.remote_releases = [{
            "body": f"<!-- pcsweep-commit:{self.commit} -->", "draft": True,
            "tag_name": "v1.1.0", "id": 7, "html_url": "existing",
        }]
        self.created = self.remote_releases.copy()
        self.publish()
        self.assertEqual(len(self.created), 1)
        self.assertEqual(self.gh_calls[0][0:2], ("release", "upload"))

    def test_corrupt_package_prevents_release_creation(self):
        self.exe.write_bytes(b"MZchanged-after-build")
        with self.assertRaisesRegex(RuntimeError, "differs"):
            self.publish()
        self.assertFalse(self.created)

    def test_missing_remote_asset_prevents_publication(self):
        with patch.object(release, "pages", return_value=[]):
            with self.assertRaisesRegex(RuntimeError, "missing or incomplete"):
                release.verify_assets("test/app", {"id": 7}, [self.exe])

    def test_version_metadata_stays_valid(self):
        self.assertEqual(release.build_tag("1.1.0+custom", "42"), "v1.1.0+custom.build.42")

    def test_updater_manifest_rejects_invalid_build_identity(self):
        with self.assertRaisesRegex(RuntimeError, "build identity"):
            release.release_assets(self.dist, "1.1.0", "v1.1.0", "bad", "42")

    def test_manifest_keeps_base_version_when_release_has_build_suffix(self):
        self.tag_exists = True
        self.publish()
        manifest = json.loads((self.dist / "update.json").read_text())
        self.assertEqual(manifest["version"], "1.1.0")
        self.assertEqual(manifest["revision"], 42)


if __name__ == "__main__":
    unittest.main()
