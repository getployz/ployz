#!/usr/bin/env python3
"""Seed a Cargo target dir from another target dir using hardlinked deps."""

import json
import os
import re
import subprocess
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: cargo-hardlink-deps.py <src-target-dir> <dst-target-dir>", file=sys.stderr)
        return 2

    src = os.path.join(sys.argv[1], "debug")
    dst = os.path.join(sys.argv[2], "debug")
    if not os.path.isdir(src):
        print(f"source debug target does not exist: {src}", file=sys.stderr)
        return 1

    registry = dependency_names()
    link_deps(src, dst, registry)
    link_fingerprints(src, dst, registry)
    link_build_outputs(src, dst, registry)
    return 0


def dependency_names() -> set[str]:
    meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1"]))
    names = set()
    for package in meta["packages"]:
        source = package.get("source") or ""
        if source.startswith(("registry+", "git+")):
            names.add(token(package["name"]))
            for target in package["targets"]:
                names.add(token(target["name"]))
    return names


def link_deps(src: str, dst: str, registry: set[str]) -> None:
    src_dir = os.path.join(src, "deps")
    dst_dir = os.path.join(dst, "deps")
    if not os.path.isdir(src_dir):
        return
    os.makedirs(dst_dir, exist_ok=True)
    for entry in os.scandir(src_dir):
        if artifact_package(entry.name) in registry:
            hardlink(entry.path, os.path.join(dst_dir, entry.name))


def link_fingerprints(src: str, dst: str, registry: set[str]) -> None:
    src_dir = os.path.join(src, ".fingerprint")
    dst_dir = os.path.join(dst, ".fingerprint")
    if not os.path.isdir(src_dir):
        return
    os.makedirs(dst_dir, exist_ok=True)
    for entry in os.scandir(src_dir):
        if fingerprint_package(entry.name) in registry:
            link_tree(entry.path, os.path.join(dst_dir, entry.name))


def link_build_outputs(src: str, dst: str, registry: set[str]) -> None:
    src_dir = os.path.join(src, "build")
    dst_dir = os.path.join(dst, "build")
    if not os.path.isdir(src_dir):
        return
    for entry in os.scandir(src_dir):
        if fingerprint_package(entry.name) in registry:
            link_tree(entry.path, os.path.join(dst_dir, entry.name))


def link_tree(src: str, dst: str) -> None:
    for root, dirs, files in os.walk(src):
        rel = os.path.relpath(root, src)
        out = dst if rel == "." else os.path.join(dst, rel)
        os.makedirs(out, exist_ok=True)
        for name in files:
            hardlink(os.path.join(root, name), os.path.join(out, name))
        sync_dir_time(root, out)
        for name in dirs:
            os.makedirs(os.path.join(out, name), exist_ok=True)


def hardlink(src: str, dst: str) -> None:
    try:
        os.link(src, dst)
    except FileExistsError:
        os.unlink(dst)
        os.link(src, dst)


def sync_dir_time(src: str, dst: str) -> None:
    stat = os.stat(src)
    os.utime(dst, ns=(stat.st_atime_ns, stat.st_mtime_ns))


def token(value: str) -> str:
    return value.replace("-", "_")


def artifact_package(name: str) -> str:
    name = re.sub(r"\.[^.]*$", "", name)
    name = re.sub(r"^lib", "", name)
    name = re.sub(r"-[^-]+$", "", name)
    return token(name)


def fingerprint_package(name: str) -> str:
    return token(re.sub(r"-[^-]+$", "", name))


if __name__ == "__main__":
    raise SystemExit(main())
