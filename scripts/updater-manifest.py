#!/usr/bin/env python3
"""Build the desktop update feed from the signed GitHub release downloads."""

import argparse
import json
import pathlib
import re
import subprocess


def channel_version(repository, channel):
    result = subprocess.run(
        ["gh", "api", "--include", f"repos/{repository}/releases/tags/{channel}"],
        capture_output=True, text=True,
    )
    status = re.match(r"HTTP/\S+\s+(\d+)", result.stdout)
    if status and status[1] == "404":
        return None
    if result.returncode or not status or status[1] != "200":
        raise RuntimeError("Cannot read the update channel; refusing to bypass version ordering")
    _, separator, body = result.stdout.partition("\n\n")
    if not separator:
        raise ValueError("Missing GitHub release response body")
    release = json.loads(body)
    if release["draft"]:
        return None
    return json.loads(release["body"])["version"]


def version_tuple(version):
    match = re.fullmatch(r"v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version)
    if not match:
        raise ValueError(f"Expected a numeric release version, got {version!r}")
    return tuple(map(int, match.groups()))


def manifest(directory, tag, repository):
    version_tuple(tag)
    if not tag.startswith("v") or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("Expected a v-prefixed tag and an owner/repository")
    platforms = {}
    for os_name, target in [("macos", "darwin"), ("linux", "linux")]:
        for arch in ["aarch64", "x86_64"]:
            suffixes = ["app.tar.gz"] if os_name == "macos" else ["AppImage", "deb"]
            for suffix in suffixes:
                name = f"init-desktop-{tag}-{os_name}-{arch}.{suffix}"
                artifact = directory / name
                if not artifact.is_file() or artifact.stat().st_size == 0:
                    raise ValueError(f"Missing or empty update artifact: {name}")
                signature = (directory / f"{name}.sig").read_text().strip()
                if not signature:
                    raise ValueError(f"Missing signature for {name}")
                key = f"{target}-{arch}"
                if os_name == "linux":
                    key += f"-{suffix.lower()}"
                platforms[key] = {
                    "url": f"https://github.com/{repository}/releases/download/{tag}/{name}",
                    "signature": signature,
                }
    return {
        "version": tag[1:],
        "notes": "Init will install this update and reopen automatically.",
        "platforms": platforms,
    }


def should_publish(candidate, previous):
    return version_tuple(candidate["version"]) >= version_tuple(previous["version"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=pathlib.Path)
    parser.add_argument("tag")
    parser.add_argument("repository")
    parser.add_argument("--previous", type=pathlib.Path)
    parser.add_argument("--channel")
    parser.add_argument("--github-output", type=pathlib.Path)
    args = parser.parse_args()
    candidate = manifest(args.directory, args.tag, args.repository)
    previous = json.loads(args.previous.read_text())["version"] if args.previous else None
    if args.channel:
        previous = channel_version(args.repository, args.channel)
    publish = previous is None or should_publish(candidate, {"version": previous})
    (args.directory / "latest.json").write_text(json.dumps(candidate, indent=2) + "\n")
    if args.github_output:
        with args.github_output.open("a") as output:
            output.write(f"publish={'true' if publish else 'false'}\n")
    print(f"Generated {candidate['version']} update feed. Publish channel: {publish}.")


if __name__ == "__main__":
    main()
