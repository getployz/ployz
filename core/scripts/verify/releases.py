"""Cache checksum-verified published runtime artifacts independently of the VM provider."""

import fcntl
import hashlib
import platform
from pathlib import Path
import re
import shutil
import tarfile
import time


def prepare(run, commands, core):
    started = time.monotonic()
    source = run["daemon"]["source"]
    if source in ["stable", "beta"]:
        major = re.search(r'^version = "(\d+)\.', (core / "Cargo.toml").read_text(), re.M).group(1)
        source = commands(["curl", "-fsSL", "--max-time", "30",
                           f"https://ployz.sh/v{major}/{source}"]).stdout.decode().strip()
    version = source.removeprefix("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise RuntimeError("Choose --daemon checkout, stable, beta or an exact release version")
    if run["daemon"]["source"] in ["stable", "beta"] and version.split(".", 1)[0] != major:
        raise RuntimeError("The release channel returned a version on another major line")
    if run["daemon"]["source"] == "stable" and "-" in version:
        raise RuntimeError("The stable channel returned a prerelease")
    artifacts = Path(run["manifest"]).parent / "artifacts" / "runtime"
    run["binaries"] = acquire(version, commands, artifacts)
    run["daemon"]["version"] = version
    run["timings"]["release_artifacts"] = round(time.monotonic() - started, 3)


def acquire(version, commands, artifacts):
    arch = {"x86_64": "amd64", "aarch64": "arm64"}.get(platform.machine())
    if not arch:
        raise RuntimeError("Release verification needs an amd64 or arm64 Linux host")
    cache = Path.home() / ".cache" / "ployz" / "verify" / "releases" / f"{version}-{arch}"
    cache.mkdir(parents=True, exist_ok=True)
    base = f"https://github.com/getployz/ployz/releases/download/v{version}/"
    artifacts.mkdir(parents=True, exist_ok=True)
    with (cache / ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        checksums = cache / "checksums.txt"
        if not checksums.exists():
            temporary = checksums.with_suffix(".next")
            commands(["curl", "-fsSL", "--max-time", "60", base + checksums.name, "-o", temporary])
            temporary.replace(checksums)
        expected = {line.split()[1]: line.split()[0] for line in checksums.read_text().splitlines()}
        hashes = {}
        for name, members in [("ployzd", ["ployzd", "ployz-uninstall"])]:
            archive = cache / f"{name}_linux_{arch}.tar.gz"
            checksum = expected.get(archive.name)
            if not checksum or not re.fullmatch(r"[0-9a-fA-F]{64}", checksum):
                raise RuntimeError(f"Missing release checksum for {archive.name}")
            if not archive.exists() or hashlib.sha256(archive.read_bytes()).hexdigest() != checksum.lower():
                temporary = archive.with_suffix(".next")
                commands(["curl", "-fsSL", "--max-time", "120", base + archive.name, "-o", temporary])
                if hashlib.sha256(temporary.read_bytes()).hexdigest() != checksum.lower():
                    raise RuntimeError(f"Release checksum mismatch: {archive.name}")
                temporary.replace(archive)
            with tarfile.open(archive) as bundle:
                for member in members:
                    entry = next((entry for entry in bundle.getmembers()
                                  if entry.isfile() and Path(entry.name).name == member), None)
                    if entry is None:
                        raise RuntimeError(f"{archive.name} is missing {member}")
                    destination = artifacts / member
                    with bundle.extractfile(entry) as incoming, destination.open("wb") as outgoing:
                        shutil.copyfileobj(incoming, outgoing)
                    destination.chmod(0o755)
                    hashes[member] = hashlib.sha256(destination.read_bytes()).hexdigest()
    return hashes
