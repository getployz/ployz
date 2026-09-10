#!/usr/bin/env python3
"""Select PR checks and verify their aggregate result without optional failures going green."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
JOBS = {"contracts", "rust-lint", "rust-tests", "compose", "sdk-types", "macos-cli", "cloud", "release-contracts"}
RUST = {"rust-lint", "rust-tests"}
CONTRACT_SCRIPTS = {
    "check-cloud-sdk-version.py", "test-cloud-sdk-version.py", "check-layer3-runner.sh",
    "run-layer3-tests.sh", "test-cli-installer.sh", "test-daemon-lifecycle.sh",
    "test-qualify-release.sh", "test-qualify-clean-init.sh", "qualify-release.sh",
    "qualify-clean-init.sh", "uninstall.sh", "stage-ployz-sh-site.sh",
    "check-release-tag.sh", "release-tag.sh", "promote-release.sh",
    "publish-github-release.sh", "pack-sdk-package.sh", "publish-sdk-package.sh",
}


def select(paths):
    selected = {"contracts"}
    for path in paths:
        if path.endswith(".md") or path == "evidence/product-paths.tsv" or path.startswith(("docs/", ".agents/", ".claude/", ".cursor/", "site/")):
            continue
        if path.startswith(".github/") or Path(path).name in {"Cargo.toml", "Cargo.lock"}:
            return JOBS.copy()
        if path.startswith("cloud/"):
            selected.add("cloud")
        elif path.startswith("crates/"):
            if path.count("/") < 2:
                return JOBS.copy()
            _, crate, relative = path.split("/", 2)
            selected |= RUST
            if crate == "ployzd":
                continue
            selected.add("macos-cli")
            if crate == "ployz-testkit":
                continue
            if crate not in {"ployz", "ployz-core", "ployz-build", "ployz-sdk", "ployz-config-wasm"}:
                return JOBS.copy()
            if not relative.startswith("tests/") or crate == "ployz-sdk":
                selected |= {"cloud", "sdk-types"}
            if relative.startswith("compose-helper/"):
                selected.add("compose")
        elif path == "install.sh" or path.startswith("scripts/qualify-release/"):
            continue
        elif path in {"scripts/build-cloud-sdk.sh", "scripts/build-config-browser.sh"}:
            selected.add("cloud")
        elif path == "scripts/check-sdk-types.sh":
            selected.add("sdk-types")
        elif path == "scripts/test-missing-ssh-client.sh":
            selected |= RUST
        elif path.startswith("scripts/") and Path(path).name in CONTRACT_SCRIPTS:
            continue
        else:
            artifacts = subprocess.check_output(
                ["bash", str(ROOT / "scripts/release-artifacts-needed.sh"), "pull_request", path], text=True
            ).strip()
            if artifacts != "true":
                return JOBS.copy()
            selected.add("release-contracts")
    return selected


def changed_paths(base, head):
    # --no-renames includes both the removed and added path, so moving a fixture into
    # production code cannot retain the fixture's smaller selection.
    output = subprocess.check_output(["git", "diff", "--name-only", "--no-renames", "-z", base, head, "--"])
    return os.fsdecode(output).rstrip("\0").split("\0") if output else []


def check_result(needs):
    changes = needs["changes"]
    if changes["result"] != "success":
        raise ValueError("check selection did not succeed")
    selected = json.loads(changes["outputs"]["jobs"])
    if not isinstance(selected, list) or not all(isinstance(job, str) for job in selected):
        raise ValueError("invalid check selection")
    if "contracts" not in selected or not set(selected) <= JOBS:
        raise ValueError("check selection is missing contracts or contains an unknown job")
    for job in sorted(JOBS):
        result = needs[job]["result"]
        allowed = {"success"} if job in selected else {"success", "skipped"}
        if result not in allowed:
            raise ValueError(f"{job}: {result} ({'required' if job in selected else 'unselected'})")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    selection = commands.add_parser("select")
    selection.add_argument("base", nargs="?", default="")
    selection.add_argument("head", nargs="?", default="HEAD")
    commands.add_parser("ready")
    args = parser.parse_args()
    if args.command == "select":
        # A manual quick run covers every normal check, without release/cluster work.
        selected = select(changed_paths(args.base, args.head)) if args.base and set(args.base) != {"0"} else JOBS - {"release-contracts"}
        output = json.dumps(sorted(selected))
        print(output)
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as file:
                file.write(f"jobs={output}\n")
        if os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as file:
                file.write("| Check | Selection |\n|---|---|\n")
                for job in sorted(JOBS):
                    file.write(f"| {job} | {'Run' if job in selected else 'Skipped: inputs unchanged'} |\n")
    else:
        needs = json.loads(os.environ["CI_NEEDS"])
        if os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as file:
                file.write("| Check | Result |\n|---|---|\n")
                for job, data in sorted(needs.items()):
                    file.write(f"| {job} | {data['result']} |\n")
        check_result(needs)
        print("All selected checks passed.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f"CI: {error}", file=sys.stderr)
        sys.exit(1)
