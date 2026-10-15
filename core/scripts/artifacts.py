#!/usr/bin/env python3
"""Content keys and a local cache for the binaries the verify harness and Cloud build from this checkout.

A key covers what the build reads: the workspace crates the artifact's crate depends on (tests, benches and
Markdown excluded), the workspace Cargo files and the recipe. Uncommitted and untracked files count, so a dirty
tree gets its own key and reverting an edit finds the earlier build again.

CI uploads each build as the GitHub Actions artifact <name>-<key>. A miss in the local cache looks there next.
"""

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import fcntl
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
import zipfile

CORE = Path(__file__).resolve().parents[1]
CACHE = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "ployz" / "artifacts"
CACHE_BYTES = int(float(os.environ.get("PLOYZ_ARTIFACT_CACHE_GB", "20")) * 2**30)
CACHE_DAYS = 30
REPOSITORY = os.environ.get("GITHUB_REPOSITORY", "getployz/ployz")


@dataclass(frozen=True)
class Kind:
    name: str  # CI uploads the artifact as <name>-<key>
    roots: tuple  # workspace crates whose closure the artifact is built from
    files: tuple  # the artifact's files, as cached and as CI uploads them
    recipe: str  # bump to invalidate every cached artifact of this kind
    inputs: tuple = ()  # other files under core/ the build reads
    generated: tuple = ()  # files the build writes into the tree: outputs, so not part of the key
    surface: tuple = ()  # what callers see: while these match main's, `fetch --stale-ok` takes main's build
    embeds: tuple = ()  # kinds built into this one, which must match main's exactly for `fetch --stale-ok`
    wire: tuple = ()  # the build generates types from its crates' files that use one of these words, so they are surface too


SDK = "crates/ployz-sdk"
SDK_FILES = ("ployz-sdk.node", "generated/payloads.d.ts", "generated/capabilities.d.ts", "generated/capabilities.js",
             "generated/capabilities.mjs", "generated/catalog.json", "generated/config-wasm.mjs")
KINDS = {
    "sdk": Kind("cloud-sdk", ("ployz-sdk", "ployz-config-wasm"), SDK_FILES,
                "sdk-v2", ("scripts/build-cloud-sdk.sh", "scripts/build-config-wasm.sh"),
                tuple(f"{SDK}/{name}" for name in SDK_FILES if name.startswith("generated/")),
                (f"{SDK}/src", f"{SDK}/index.d.ts", f"{SDK}/index.js", f"{SDK}/config.d.ts",
                 "crates/ployz-store/src/settings", "crates/ployz-store/src/variables.rs",
                 "crates/ployz-core/src/rpc_catalog.rs"),
                ("config-wasm",), ("ts_rs", "TS")),
    # Part of the SDK, cached on its own so the addon can rebuild without it. CI never uploads it alone.
    "config-wasm": Kind("config-wasm", ("ployz-config-wasm",), ("generated/config-wasm.mjs",), "wasm-v1",
                        ("scripts/build-config-wasm.sh",)),
    "ployzd": Kind("ployzd", ("ployzd",), ("ployzd",), "dev-debug0-strip-v1"),
    "ployz": Kind("ployz", ("ployz",), ("ployz",), "dev-debug0-strip-v1"),
}
WORKSPACE = ("Cargo.toml", "Cargo.lock", ".cargo", "rust-toolchain.toml")
# Files outside core/ a crate's build script embeds, keyed by crate.
EMBEDDED = {"ployz-store": ("../docs/user",)}


@dataclass(frozen=True)
class Key:
    kind: str
    value: str
    crates: dict  # crate -> digest of its sources, for naming what differs from another build
    extra: str = ""

    @property
    def artifact(self):
        return f"{KINDS[self.kind].name}-{self.value}"


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


def mentioning(words, paths, rev=None):
    """Files under paths, at rev or in the working tree, that contain one of words."""
    if not words:
        return []
    patterns = [arg for word in words for arg in ("-e", word)]
    where = [*patterns, rev] if rev else ["--untracked", *patterns]
    found = subprocess.run(["git", "-c", "grep.fullName=false", "grep", "-l", "-F", "-w", *where, "--", *paths],
                           cwd=CORE, capture_output=True)
    if found.returncode > 1:
        raise subprocess.CalledProcessError(found.returncode, found.args, found.stdout, found.stderr)
    return [line.removeprefix(f"{rev}:") if rev else line for line in found.stdout.decode().splitlines()]


def digest(rows):
    return hashlib.sha256("".join(f"{path} {rows[path]}\n" for path in sorted(rows)).encode()).hexdigest()


def key(kind, extra="", rev=None):
    """The key of kind's artifact built from rev, or from the working tree, with extra build options."""
    spec = KINDS[kind]
    crates = closure(spec.roots, rev)
    embedded = [path for name in crates for path in EMBEDDED.get(name, ())]
    files = entries([*WORKSPACE, *spec.inputs, *crates.values(), *embedded], rev)
    for path in spec.generated:
        files.pop(path, None)
    by_crate = {name: digest({path: blob for path, blob in files.items()
                              if path.startswith(directory + "/") and counts(path, directory)})
                for name, directory in crates.items()}
    shared = {path: blob for path, blob in files.items() if not path.startswith("crates/")}
    recipe = [spec.recipe, kind, f"{platform.system()}-{platform.machine()}", extra, digest(shared),
              *(f"{name} {by_crate[name]}" for name in sorted(by_crate))]
    return Key(kind, hashlib.sha256("\n".join(recipe).encode()).hexdigest()[:32], by_crate, extra)


def surface(kind, rev=None):
    spec = KINDS[kind]
    files = entries([*spec.surface, *mentioning(spec.wire, closure(spec.roots, rev).values(), rev)], rev)
    for path in spec.generated:
        files.pop(path, None)
    return digest(files)


def revision():
    head = git("rev-parse", "HEAD").decode().strip()
    return head + ("-dirty" if git("status", "--porcelain", "--", ".") else "")


def run(args, **kwargs):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE, **kwargs)


# Harness builds strip their binaries, so DWARF is wasted work: a third of an edit rebuild. kache
# strips incremental flags unless told to keep them, which makes a one-line edit recompile whole crates.
HARNESS_ENV = {"CARGO_PROFILE_DEV_DEBUG": "0", "KACHE_PRESERVE_INCREMENTAL": "1"}
BUILD_SLOTS = 2  # concurrent harness builds per machine; worktrees beyond that wait rather than thrash the cores


@contextmanager
def build_slot():
    """Hold one of the machine's BUILD_SLOTS until the block ends, waiting for one to free up."""
    slots = CACHE.parent / "build-slots"
    slots.mkdir(parents=True, exist_ok=True)
    waiting = False
    while True:
        for index in range(BUILD_SLOTS):
            with (slots / str(index)).open("w") as slot:
                try:
                    fcntl.flock(slot, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    continue
                yield
                return
        if not waiting:
            print(f"waiting for one of {BUILD_SLOTS} build slots", file=sys.stderr)
            waiting = True
        time.sleep(1)


def harness_build(args, commands=run, **kwargs):
    """Run a harness build in a build slot, at low priority."""
    with build_slot():
        return commands(["nice", "-n", "10", *args], env={**os.environ, **HARNESS_ENV}, **kwargs)


def binaries(destination, names, cargo_args=(), commands=run):
    """Put the named stripped dev binaries into destination: from the cache, or from one cargo build of the misses."""
    extra = shlex.join(cargo_args)
    wanted = {name: key(name, extra) for name in names}
    destination.mkdir(parents=True, exist_ok=True)
    missing = [name for name, found in wanted.items() if not fetch(found, destination)]
    if not missing:
        return
    harness_build(["cargo", "build", "--locked", *(arg for name in missing for arg in ["-p", name]), *cargo_args],
                  commands, cwd=CORE, timeout=1800)
    metadata = commands(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=CORE)
    target = Path(json.loads(metadata.stdout)["target_directory"]) / "debug"
    for name in missing:
        shutil.copy2(target / name, destination / name)
        commands(["strip", "--strip-debug", destination / name])
        # An edit made while cargo ran may or may not be in the binary, so it gets no key.
        if key(name, extra) == wanted[name]:
            store(wanted[name], destination, revision=revision())
        print(f"{name}: built {wanted[name].value[:8]}", file=sys.stderr)


def gh(*args, **kwargs):
    return subprocess.run(["gh", "api", *args], check=True, timeout=600, **kwargs)


def lookup(found):
    """found's newest live CI upload from main. Any branch or fork pull request can upload under a key's name, unreviewed."""
    try:
        listing = gh(f"repos/{REPOSITORY}/actions/artifacts?name={found.artifact}&per_page=100", capture_output=True)
    except (OSError, subprocess.SubprocessError):
        return None  # no gh, not signed in, or offline: a miss
    for artifact in json.loads(listing.stdout)["artifacts"]:
        run = artifact.get("workflow_run") or {}
        if (not artifact["expired"] and run.get("head_branch") == "main"
                and run.get("head_repository_id") == run.get("repository_id")):
            return artifact
    return None


def download(found):
    """Put found's CI upload into the local cache. Returns whether CI had one."""
    artifact = lookup(found)
    if artifact is None:
        return False
    with tempfile.TemporaryDirectory() as scratch:
        archive = Path(scratch) / "artifact.zip"
        try:
            with archive.open("wb") as out:
                gh(f"repos/{REPOSITORY}/actions/artifacts/{artifact['id']}/zip", stdout=out)
        except subprocess.SubprocessError:
            print(f"{found.artifact}: download failed, so it counts as a miss", file=sys.stderr)
            return False
        actual = "sha256:" + hashlib.sha256(archive.read_bytes()).hexdigest()
        if artifact.get("digest", actual) != actual:
            raise SystemExit(f"{found.artifact}: downloaded {actual}, GitHub recorded {artifact['digest']}")
        with zipfile.ZipFile(archive) as files:
            files.extractall(Path(scratch) / "files")
        run = artifact["workflow_run"]
        store(found, Path(scratch) / "files", revision=run["head_sha"], run=run["id"])
    return True


def fetch(found, destination, *, announce=True):
    """Copy an artifact's files into destination, from the local cache or else from CI. Returns its metadata, or None on a miss."""
    entry = CACHE / found.kind / found.value
    downloaded = not (entry / "meta.json").exists()
    if downloaded and not download(found):
        return None
    meta = json.loads((entry / "meta.json").read_text())
    origin = f"github run {meta.get('run')}" if downloaded else f"local {found.value[:8]}"
    if announce:
        print(f"{found.kind}: hit {origin} ({meta.get('revision', 'unknown')[:12]})", file=sys.stderr)
    destination.mkdir(parents=True, exist_ok=True)
    for name in KINDS[found.kind].files:
        # Replace, don't overwrite: macOS caches a code signature per inode and SIGKILLs a stale one.
        temporary = (destination / name).with_name(f".{Path(name).name}.{os.getpid()}")
        temporary.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(entry / name, temporary)
        temporary.replace(destination / name)
    os.utime(entry)
    return meta


def nearest_main(found, depth=20):
    """The newest main commit, at or before this checkout's fork point, with a build of found's kind: (commit, key)."""
    try:
        base = git("merge-base", "HEAD", "origin/main").decode().strip()
    except subprocess.CalledProcessError:
        return None
    seen = set()
    for rev in git("rev-list", "--first-parent", f"--max-count={depth}", base).decode().split():
        candidate = key(found.kind, found.extra, rev)
        if candidate.value in seen:
            continue
        seen.add(candidate.value)
        if (CACHE / found.kind / candidate.value / "meta.json").exists() or lookup(candidate):
            return rev, candidate
    return None


def fetch_main(found, destination, hint, *, same_surface=False):
    """Fetch main's build in place of found and name the crates that differ. Returns its metadata, or None.

    same_surface refuses main's build unless the kind's surface and embedded kinds match the checkout's.
    """
    nearest = nearest_main(found)
    if nearest is None:
        print(f"{found.kind}: main has no build in its last 20 commits", file=sys.stderr)
        return None
    rev, candidate = nearest
    if same_surface and surface(found.kind) != surface(found.kind, rev):
        print(f"{found.kind}: interface differs from main@{rev[:9]}, so building", file=sys.stderr)
        return None
    if same_surface and any(key(kind).value != key(kind, rev=rev).value for kind in KINDS[found.kind].embeds):
        print(f"{found.kind}: {', '.join(KINDS[found.kind].embeds)} differs from main@{rev[:9]}, so building", file=sys.stderr)
        return None
    meta = fetch(candidate, destination, announce=False)
    if meta is None:
        return None
    differ = sorted(name for name in found.crates.keys() | candidate.crates.keys()
                    if found.crates.get(name) != candidate.crates.get(name))
    what = f"{', '.join(differ)} differ{'s' if len(differ) == 1 else ''}" if differ else "workspace files differ"
    print(f"{found.kind}: stale main@{rev[:9]} ({what}) -- {hint}", file=sys.stderr)
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
    for action in ["key", "lookup", "fetch", "store"]:
        command = actions.add_parser(action)
        command.add_argument("kind", choices=KINDS)
        command.add_argument("--extra", default="", help="Build options that change the artifact, such as features")
        if action == "lookup":
            command.description = "Print the CI artifact's name and the run that uploaded it, empty if none, as step outputs."
        elif action == "key":
            command.add_argument("--rev", help="Key the build of this commit instead of the working tree")
        else:
            command.add_argument("directory", type=Path, help="Where the artifact's files go, or come from")
        if action == "store":
            command.add_argument("--expect", help="The key taken before the build: an edit made since skips the store")
        if action == "fetch":
            command.add_argument("--stale-ok", metavar="HINT",
                                 help="On a miss, take main's build while the kind's surface matches; HINT says how to build instead")
    command = actions.add_parser("binaries", help="Fetch or build stripped dev binaries, as the verify harness runs")
    command.add_argument("directory", type=Path)
    command.add_argument("kinds", nargs="+", choices=["ployz", "ployzd"])
    command = actions.add_parser("build", help="Run a harness build: in one of the machine's build slots, at low priority")
    command.add_argument("command", nargs=argparse.REMAINDER)
    actions.add_parser("prune")
    args = parser.parse_args()
    if args.action == "prune":
        prune()
        return 0
    if args.action == "build":
        return harness_build(args.command, subprocess.run).returncode
    if args.action == "binaries":
        binaries(args.directory, args.kinds)
        return 0
    found = key(args.kind, args.extra, getattr(args, "rev", None))
    if args.action == "key":
        print(found.value)
    elif args.action == "lookup":
        artifact = lookup(found)
        print(f"name={found.artifact}\nrun-id={artifact['workflow_run']['id'] if artifact else ''}")
    elif args.action == "store":
        if args.expect and args.expect != found.value:
            print(f"{args.kind}: sources changed during the build, not caching it", file=sys.stderr)
        else:
            store(found, args.directory, revision=revision())
    elif fetch(found, args.directory) is None and not (
            args.stale_ok and fetch_main(found, args.directory, args.stale_ok, same_surface=True)):
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
