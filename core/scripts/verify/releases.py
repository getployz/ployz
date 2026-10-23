"""Cache checksum-verified published runtime artifacts independently of the VM provider."""

import fcntl
import hashlib
import os
import platform
from pathlib import Path
import re
import shutil
import tarfile


def override(name, default):
    """install.sh's `${NAME:-default}`: an unset or empty variable is its default."""
    return (os.environ.get(name) or default).rstrip("/")


REPOSITORY = "https://github.com/getployz/ployz"


def download_base(version):
    """Where a release's assets download from: PLOYZ_GITHUB_URL's releases, as in install.sh."""
    return f"{override('PLOYZ_GITHUB_URL', REPOSITORY)}/releases/download/v{version}/"


def staged():
    """A cache suffix for releases taken from PLOYZ_GITHUB_URL: a staged copy of a version is a
    different release until it is published, so it caches apart."""
    repository = override("PLOYZ_GITHUB_URL", REPOSITORY)
    return "" if repository == REPOSITORY else "-" + hashlib.sha256(repository.encode()).hexdigest()[:12]


def mirror_environment():
    """`NAME=value` for the guest installer, so it downloads from the same staged copy."""
    return [] if not staged() else [f"PLOYZ_GITHUB_URL={override('PLOYZ_GITHUB_URL', REPOSITORY)}"]


def resolve(source, commands, core):
    """The exact release a daemon source names: stable, beta or a version."""
    if source in ["stable", "beta"]:
        major = re.search(r'^version = "(\d+)\.', (core / "Cargo.toml").read_text(), re.M).group(1)
        version = commands(["curl", "-fsSL", "--max-time", "30",
                            f"{override('PLOYZ_CHANNEL_URL', 'https://ployz.sh')}/v{major}/{source}"]).stdout.decode().strip().removeprefix("v")
        if version.split(".", 1)[0] != major:
            raise RuntimeError("The release channel returned a version on another major line")
        if source == "stable" and "-" in version:
            raise RuntimeError("The stable channel returned a prerelease")
    else:
        version = source.removeprefix("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise RuntimeError(f"Unknown daemon source {source!r}: choose checkout, stable, beta or an exact release version")
    return version


# Each published Linux archive and the executables taken from it.
ARCHIVES = {"ployzd": ["ployzd", "ployz-uninstall"], "ployz": ["ployz"]}


def acquire(version, commands, artifacts, name="ployzd"):
    arch = {"x86_64": "amd64", "aarch64": "arm64"}.get(platform.machine())
    if not arch:
        raise RuntimeError("Release verification needs an amd64 or arm64 Linux host")
    base = download_base(version)
    cache = Path.home() / ".cache" / "ployz" / "verify" / "releases" / f"{version}-{arch}{staged()}"
    cache.mkdir(parents=True, exist_ok=True)
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
            for member in ARCHIVES[name]:
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
