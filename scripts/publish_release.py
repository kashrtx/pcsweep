"""Publish each tested main push; a rerun resumes its draft or verifies its release."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from urllib.parse import quote
import zipfile


class GitHubError(RuntimeError):
    pass


def gh(*args, payload=None):
    result = subprocess.run(
        ["gh", *args],
        input=json.dumps(payload) if payload is not None else None,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode:
        raise GitHubError(result.stderr.strip() or "GitHub CLI failed")
    return json.loads(result.stdout) if result.stdout.strip() else None


def api(route, payload=None, method="GET"):
    args = ["api", route, "--method", method]
    if payload is not None:
        args.extend(["--input", "-"])
    return gh(*args, payload=payload)


def pages(route):
    return [item for page in gh("api", route, "--paginate", "--slurp") for item in page]


def existing_tag(repo, tag):
    try:
        return api(f"repos/{repo}/git/ref/tags/{quote(tag, safe='')}")
    except GitHubError as error:
        if "HTTP 404" in str(error):
            return None
        raise


def build_tag(version, run_number):
    separator = "." if "+" in version else "+"
    return f"v{version}{separator}build.{run_number}"


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def validate_package(dist, version):
    executable = dist / "PCSweep.exe"
    archive = dist / f"PCSweep-{version}-windows-x64.zip"
    if not executable.is_file() or not archive.is_file():
        raise RuntimeError("The tested executable or portable ZIP is missing")
    with executable.open("rb") as source:
        if source.read(2) != b"MZ":
            raise RuntimeError("PCSweep.exe is not a Windows executable")
    with zipfile.ZipFile(archive) as package:
        required = {"PCSweep.exe", "README.md", "LICENSE", "RELEASE_NOTES.md"}
        if not required.issubset(package.namelist()) or package.testzip() is not None:
            raise RuntimeError("Portable ZIP is incomplete or corrupt")
        if hashlib.sha256(package.read("PCSweep.exe")).hexdigest() != digest(executable):
            raise RuntimeError("The ZIP executable differs from the tested executable")


def release_assets(dist, version, tag):
    original = dist / f"PCSweep-{version}-windows-x64.zip"
    archive = dist / f"PCSweep-{tag.removeprefix('v')}-windows-x64.zip"
    if original != archive:
        original.rename(archive)
    assets = [dist / "PCSweep.exe", archive]
    checksums = dist / "SHA256SUMS.txt"
    checksums.write_text(
        "".join(f"{digest(path)}  {path.name}\n" for path in assets), encoding="ascii"
    )
    return [*assets, checksums]


def verify_assets(repo, release, assets):
    remote = {
        item["name"]: item
        for item in pages(f"repos/{repo}/releases/{release['id']}/assets?per_page=100")
    }
    for asset in assets:
        uploaded = remote.get(asset.name)
        if uploaded is None or uploaded["size"] != asset.stat().st_size:
            raise RuntimeError(f"Release asset missing or incomplete: {asset.name}")
        checksum = uploaded.get("digest")
        if checksum and checksum != f"sha256:{digest(asset)}":
            raise RuntimeError(f"Release checksum mismatch: {asset.name}")


def verify_published(repo, release):
    # Rebuilding the same commit may change ZIP timestamps. Verify the existing
    # published downloads against their own checksums instead of replacing them.
    archive = f"PCSweep-{release['tag_name'].removeprefix('v')}-windows-x64.zip"
    names = ["PCSweep.exe", archive, "SHA256SUMS.txt"]
    with tempfile.TemporaryDirectory(prefix="pcsweep-release-") as folder:
        args = ["release", "download", release["tag_name"], "--repo", repo, "--dir", folder]
        for name in names:
            args.extend(["--pattern", name])
        gh(*args)
        paths = [Path(folder) / name for name in names]
        verify_assets(repo, release, paths)
        expected = "".join(f"{digest(path)}  {path.name}\n" for path in paths[:2])
        if paths[2].read_text(encoding="ascii") != expected:
            raise RuntimeError("Published SHA256SUMS.txt does not match release downloads")


def publish(version, dist, repo, commit, run_number, run_url, notes):
    validate_package(dist, version)
    marker = f"<!-- pcsweep-commit:{commit} -->"
    releases = pages(f"repos/{repo}/releases?per_page=100")
    release = next((item for item in releases if marker in (item.get("body") or "")), None)
    if release is not None and not release["draft"]:
        verify_published(repo, release)
        return release["html_url"]
    if release is None:
        tag = f"v{version}"
        if existing_tag(repo, tag) or any(item["tag_name"] == tag for item in releases):
            tag = build_tag(version, run_number)
        payload = {
            "tag_name": tag,
            "target_commitish": commit,
            "name": f"PCSweep {tag.removeprefix('v')}",
            "body": f"{notes.rstrip()}\n\nBuilt from `{commit}`. [Build details]({run_url}).\n\n{marker}\n",
            "draft": True,
            "prerelease": False,
        }
        try:
            release = api(f"repos/{repo}/releases", payload, "POST")
        except GitHubError as error:
            # Another push may reserve the base version while this job is
            # building. Every run has a unique fallback, without dropping runs.
            if "HTTP 422" not in str(error) or tag != f"v{version}":
                raise
            payload["tag_name"] = build_tag(version, run_number)
            payload["name"] = f"PCSweep {payload['tag_name'].removeprefix('v')}"
            release = api(f"repos/{repo}/releases", payload, "POST")

    tag = release["tag_name"]
    assets = release_assets(dist, version, tag)
    if release["draft"]:
        # Only drafts may be overwritten. A retry after upload resumes safely;
        # already published downloads remain immutable.
        gh("release", "upload", tag, "--repo", repo, "--clobber", *map(str, assets))
        verify_assets(repo, release, assets)
        head = api(f"repos/{repo}/git/ref/heads/main")["object"]["sha"]
        release = api(
            f"repos/{repo}/releases/{release['id']}",
            {"draft": False, "prerelease": False, "make_latest": "true" if head == commit else "false"},
            "PATCH",
        )
    return release["html_url"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--dist", type=Path, default=Path("dist"))
    args = parser.parse_args()
    repo = os.environ["GITHUB_REPOSITORY"]
    url = publish(
        args.version,
        args.dist,
        repo,
        os.environ["GITHUB_SHA"],
        os.environ["GITHUB_RUN_NUMBER"],
        f"{os.environ['GITHUB_SERVER_URL']}/{repo}/actions/runs/{os.environ['GITHUB_RUN_ID']}",
        Path("RELEASE_NOTES.md").read_text(encoding="utf-8"),
    )
    print(f"Release verified: {url}")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as output:
            output.write(f"Published and verified [PCSweep release]({url}) with executable, portable ZIP and SHA-256 checksums.\n")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, zipfile.BadZipFile) as error:
        print(f"Release failed: {error}", file=sys.stderr)
        sys.exit(1)
