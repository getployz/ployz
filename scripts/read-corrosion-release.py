#!/usr/bin/env python3
"""Validate and select one platform from corrosion-release.json."""

import json
import re
import sys
from pathlib import Path


PLATFORMS = {
    "linux-amd64": "corrosion-x86_64-unknown-linux-gnu.tar.gz",
    "linux-arm64": "corrosion-aarch64-unknown-linux-gnu.tar.gz",
}
FIELDS = (
    "release_tag",
    "archive_name",
    "archive_url",
    "archive_sha256",
    "embedded_version",
)


def fail(message: str) -> None:
    raise SystemExit(message)


def exact_object(value: object, fields: set[str], label: str) -> dict[str, object]:
    if not isinstance(value, dict) or set(value) != fields:
        fail(f"corrosion release manifest {label} is invalid")
    return value


def nonempty_text(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        fail(f"corrosion release manifest {label} is not non-empty text")
    return value


def read_manifest(path: Path, selected_platform: str) -> dict[str, str]:
    try:
        with path.open(encoding="utf-8") as source:
            manifest = json.load(source)
    except (OSError, json.JSONDecodeError) as error:
        fail(f"read corrosion release manifest {path}: {error}")

    manifest = exact_object(
        manifest,
        {"release_tag", "platforms", "embedded_version"},
        "root",
    )
    release_tag = nonempty_text(manifest["release_tag"], "release_tag")
    if re.fullmatch(r"v[0-9A-Za-z][0-9A-Za-z._-]*", release_tag) is None:
        fail("corrosion release manifest release_tag is invalid")
    embedded_version = nonempty_text(manifest["embedded_version"], "embedded_version")
    if re.fullmatch(r"corrosion [^\t\r\n]+", embedded_version) is None:
        fail("corrosion release manifest embedded_version is invalid")

    platforms = exact_object(manifest["platforms"], set(PLATFORMS), "platforms")
    for platform, expected_name in PLATFORMS.items():
        entry = exact_object(platforms[platform], {"archive"}, f"entry {platform}")
        archive = exact_object(
            entry["archive"],
            {"name", "url", "sha256"},
            f"archive {platform}",
        )
        name = nonempty_text(archive["name"], f"archive name for {platform}")
        url = nonempty_text(archive["url"], f"archive URL for {platform}")
        sha256 = nonempty_text(archive["sha256"], f"archive SHA-256 for {platform}")
        if name != expected_name:
            fail(f"corrosion release manifest archive name for {platform} is not canonical")
        expected_url = (
            "https://github.com/superfly/corrosion/releases/download/"
            f"{release_tag}/{expected_name}"
        )
        if url != expected_url:
            fail(f"corrosion release manifest URL for {platform} is not canonical")
        if re.fullmatch(r"[0-9a-f]{64}", sha256) is None:
            fail(f"corrosion release manifest SHA-256 for {platform} is invalid")

    if selected_platform not in PLATFORMS:
        fail(f"corrosion release manifest has no pin for {selected_platform}")
    selected_archive = platforms[selected_platform]["archive"]
    return {
        "release_tag": release_tag,
        "archive_name": selected_archive["name"],
        "archive_url": selected_archive["url"],
        "archive_sha256": selected_archive["sha256"],
        "embedded_version": embedded_version,
    }


def main() -> None:
    if len(sys.argv) not in (3, 4):
        fail(
            "usage: scripts/read-corrosion-release.py "
            "<manifest> <linux-amd64|linux-arm64> [field]"
        )
    values = read_manifest(Path(sys.argv[1]), sys.argv[2])
    if len(sys.argv) == 4:
        field = sys.argv[3]
        if field not in FIELDS:
            fail(f"unknown corrosion release field: {field}")
        print(values[field])
        return
    for field in FIELDS:
        print(values[field])


if __name__ == "__main__":
    main()
