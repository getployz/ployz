#!/usr/bin/env python3
"""Content keys and a local cache for the binaries the verify harness and Cloud build from this checkout.

A key covers what the build reads: the workspace crates the artifact's crate depends on (tests, benches and
Markdown excluded), the workspace Cargo files and the recipe. Uncommitted and untracked files count, so a dirty
tree gets its own key and reverting an edit finds the earlier build again.
"""

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib

CORE = Path(__file__).resolve().parents[1]
CACHE = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "ployz" / "artifacts"
CACHE_BYTES = int(float(os.environ.get("PLOYZ_ARTIFACT_CACHE_GB", "20")) * 2**30)
CACHE_DAYS = 30


@dataclass(frozen=True)
class Kind:
    roots: tuple  # workspace crates whose closure the artifact is built from
    files: tuple  # the artifact's files, as cached and as CI uploads them
    recipe: str  # bump to invalidate every cached artifact of this kind
    inputs: tuple = ()  # other files under core/ the build reads
    generated: tuple = ()  # files the build writes into the tree: outputs, so not part of the key


KINDS = {
    "sdk": Kind(("ployz-sdk", "ployz-config-wasm"), ("ployz-sdk.node", "generated/config-wasm.mjs"), "sdk-v1",
                ("scripts/build-cloud-sdk.sh", "scripts/build-config-wasm.sh"),
                ("crates/ployz-sdk/generated/config-wasm.mjs",)),
    "ployzd": Kind(("ployzd",), ("ployzd",), "dev-debug0-strip-v1"),
    "ployz": Kind(("ployz",), ("ployz",), "dev-debug0-strip-v1"),
}
WORKSPACE = ("Cargo.toml", "Cargo.lock", ".cargo", "rust-toolchain.toml")


@dataclass(frozen=True)
class Key:
    kind: str
    value: str
    crates: dict  # crate -> digest of its sources, for naming what differs from another build


def git(*args, **kwargs):
    return subprocess.run(["git", *args], cwd=CORE, check=True, capture_output=True, **kwargs).stdout


def crate_dependencies(manifest):
    """Workspace crates a crate depends on to build: normal and build dependencies on every target."""
    tables = [manifest]
    tables += manifest.get("target", {}).values()
    found = set()
    for table in tables:
        for section in ["dependencies", "build-dependencies"]:
            for name, spec in table.get(section, {}).items():
                found.add(spec.get("package", name) if isinstance(spec, dict) else name)
    return found


def closure(roots, rev=None):
    def read(path):
        return tomllib.loads(git("show", f"{rev}:./{path}").decode() if rev else (CORE / path).read_text())
    members = read("Cargo.toml")["workspace"]["members"]
    crates = {}
    for member in members:
        manifest = read(f"{member}/Cargo.toml")
        crates[manifest["package"]["name"]] = (member, crate_dependencies(manifest))
    seen, pending = set(), list(roots)
    while pending:
        name = pending.pop()
        if name not in seen:
            seen.add(name)
            pending += [dependency for dependency in crates[name][1] if dependency in crates]
    return {name: crates[name][0] for name in seen}


def entries(paths, rev=None):
    """(path, mode and blob) for every file under paths, at rev or in the working tree."""
    paths = [path for path in paths if rev or (CORE / path).exists()]
    if rev:
        listing = git("ls-tree", "-r", "-z", rev, "--", *paths)
        rows = [line.split(b"\t", 1) for line in listing.split(b"\0") if line]
        return {path.decode(): head.split()[0].decode() + " " + head.split()[2].decode() for head, path in rows}
    # A scratch copy of the index sees untracked and modified files without touching the real one.
    with tempfile.TemporaryDirectory() as scratch:
        index = Path(scratch) / "index"
        shutil.copyfile(CORE / git("rev-parse", "--git-path", "index").decode().strip(), index)
        env = {**os.environ, "GIT_INDEX_FILE": str(index)}
        git("add", "-A", "--", *paths, env=env)
        listing = git("ls-files", "-s", "-z", "--", *paths, env=env)
    rows = [line.split(b"\t", 1) for line in listing.split(b"\0") if line]
    return {path.decode(): head.split()[0].decode() + " " + head.split()[1].decode() for head, path in rows}


def counts(path, crate_dir):
    inside = path[len(crate_dir) + 1:]
    return not (inside.split("/", 1)[0] in ["tests", "benches"] or path.endswith(".md"))


def digest(rows):
    return hashlib.sha256("".join(f"{path} {rows[path]}\n" for path in sorted(rows)).encode()).hexdigest()


def key(kind, extra="", rev=None):
    """The key of kind's artifact built from rev, or from the working tree, with extra build options."""
    spec = KINDS[kind]
    crates = closure(spec.roots, rev)
    files = entries([*WORKSPACE, *spec.inputs, *crates.values()], rev)
    for path in spec.generated:
        files.pop(path, None)
    by_crate = {name: digest({path: blob for path, blob in files.items()
                              if path.startswith(directory + "/") and counts(path, directory)})
                for name, directory in crates.items()}
    shared = {path: blob for path, blob in files.items() if not path.startswith("crates/")}
    recipe = [spec.recipe, kind, f"{platform.system()}-{platform.machine()}", extra, digest(shared),
              *(f"{name} {by_crate[name]}" for name in sorted(by_crate))]
    return Key(kind, hashlib.sha256("\n".join(recipe).encode()).hexdigest()[:32], by_crate)


def revision():
    head = git("rev-parse", "HEAD").decode().strip()
    return head + ("-dirty" if git("status", "--porcelain", "--", ".") else "")


def run(args, **kwargs):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, **kwargs)


def binaries(destination, names, cargo_args=(), commands=run):
    """Put the named stripped dev binaries into destination: from the cache, or from one cargo build of the misses."""
    extra = shlex.join(cargo_args)
    wanted = {name: key(name, extra) for name in names}
    destination.mkdir(parents=True, exist_ok=True)
    missing = [name for name, found in wanted.items() if not fetch(found, destination)]
    if not missing:
        return
    # The binaries are stripped, so DWARF is wasted work: a third of an edit rebuild.
    commands(["cargo", "build", "--locked", *(arg for name in missing for arg in ["-p", name]), *cargo_args],
             cwd=CORE, timeout=1800, env={**os.environ, "CARGO_PROFILE_DEV_DEBUG": "0"})
    metadata = commands(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=CORE)
    target = Path(json.loads(metadata.stdout)["target_directory"]) / "debug"
    for name in missing:
        shutil.copy2(target / name, destination / name)
        commands(["strip", "--strip-debug", destination / name])
        # An edit made while cargo ran may or may not be in the binary, so it gets no key.
        if key(name, extra) == wanted[name]:
            store(wanted[name], destination, revision=revision())
        print(f"{name}: built {wanted[name].value[:8]}", file=sys.stderr)


def fetch(found, destination):
    """Copy a cached artifact's files into destination. Returns its metadata, or None on a miss."""
    entry = CACHE / found.kind / found.value
    try:
        meta = json.loads((entry / "meta.json").read_text())
    except (OSError, ValueError):
        return None
    print(f"{found.kind}: hit local {found.value[:8]} ({meta.get('revision', 'unknown')[:12]})", file=sys.stderr)
    destination.mkdir(parents=True, exist_ok=True)
    for name in KINDS[found.kind].files:
        # Replace, don't overwrite: macOS caches a code signature per inode and SIGKILLs a stale one.
        temporary = (destination / name).with_name(f".{Path(name).name}.{os.getpid()}")
        temporary.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(entry / name, temporary)
        temporary.replace(destination / name)
    os.utime(entry)
    return meta


def store(found, source, **meta):
    """Cache the files of a finished build, found in source, under its key."""
    kind_dir = CACHE / found.kind
    kind_dir.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(dir=kind_dir, prefix=".staging-"))
    for name in KINDS[found.kind].files:
        (staging / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source / name, staging / name)
    meta = {"kind": found.kind, "key": found.value, "crates": found.crates, "built_at": time.time(), **meta}
    (staging / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
    try:
        staging.rename(kind_dir / found.value)
    except OSError:
        shutil.rmtree(staging)  # another build stored the same key first
    prune()


def prune():
    """Drop entries unused for CACHE_DAYS, then the least recently used beyond CACHE_BYTES."""
    stale = time.time() - CACHE_DAYS * 86400
    kept = []
    for entry in CACHE.glob("*/*"):
        if not entry.name.startswith(".") and entry.is_dir():
            if entry.stat().st_mtime < stale:
                shutil.rmtree(entry, ignore_errors=True)
            else:
                kept.append((entry.stat().st_mtime, sum(file.stat().st_size for file in entry.rglob("*") if file.is_file()), entry))
    total = sum(size for _, size, _ in kept)
    for _, size, entry in sorted(kept):
        if total <= CACHE_BYTES:
            break
        shutil.rmtree(entry, ignore_errors=True)
        total -= size


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    for action in ["key", "fetch", "store"]:
        command = actions.add_parser(action)
        command.add_argument("kind", choices=KINDS)
        command.add_argument("--extra", default="", help="Build options that change the artifact, such as features")
        if action == "key":
            command.add_argument("--rev", help="Key the build of this commit instead of the working tree")
        else:
            command.add_argument("directory", type=Path, help="Where the artifact's files go, or come from")
        if action == "store":
            command.add_argument("--expect", help="The key taken before the build: an edit made since skips the store")
    command = actions.add_parser("binaries", help="Fetch or build stripped dev binaries, as the verify harness runs")
    command.add_argument("directory", type=Path)
    command.add_argument("kinds", nargs="+", choices=["ployz", "ployzd"])
    actions.add_parser("prune")
    args = parser.parse_args()
    if args.action == "prune":
        prune()
        return 0
    if args.action == "binaries":
        binaries(args.directory, args.kinds)
        return 0
    found = key(args.kind, args.extra, getattr(args, "rev", None))
    if args.action == "key":
        print(found.value)
    elif args.action == "store":
        if args.expect and args.expect != found.value:
            print(f"{args.kind}: sources changed during the build, not caching it", file=sys.stderr)
        else:
            store(found, args.directory, revision=revision())
    elif fetch(found, args.directory) is None:
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
