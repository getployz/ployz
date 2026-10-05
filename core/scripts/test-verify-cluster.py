#!/usr/bin/env python3
"""Check compilation/VM overlap, real two-Machine readiness and cleanup.

Usage: test-verify-cluster.py [daemon] [--cloud]. --cloud pairs through this checkout's dashboard and checks that
down stops it."""

import json
from pathlib import Path
import subprocess
import sys

core = Path(__file__).resolve().parents[1]
helper = core / "scripts" / "verify-cluster"
cloud = "--cloud" in sys.argv[1:]
positional = [arg for arg in sys.argv[1:] if arg != "--cloud"]
daemon = positional[0] if positional else "stable"
result = subprocess.run([helper, "up", "--machines", "2", "--daemon", daemon, *(["--cloud"] if cloud else [])],
                        cwd=core, stdout=subprocess.PIPE, text=True, check=True)
manifest = Path(result.stdout.strip())
try:
    subprocess.run([helper, "doctor", manifest], check=True, stdout=subprocess.DEVNULL)
    events = [json.loads(line) for line in
              (manifest.parent / "evidence" / "commands.jsonl").read_text().splitlines()]

    def interval(event):
        start = int(event["artifact"].split("-", 1)[0]) / 1e9
        return start, start + event["seconds"]

    build_start = interval(next(event for event in events
                                if event["command"][:2] == ["cargo", "build"]))[0]
    build_end = interval(next(event for event in events
                              if event["command"] == ["git", "rev-parse", "HEAD"]))[1]
    overlap = max((max(0, min(build_end, end) - max(build_start, start))
                   for event in events if "incus" in event["command"]
                   for start, end in [interval(event)]), default=0)
    assert overlap > 0, "Compilation and VM preparation ran serially"
    run = json.loads(manifest.read_text())
    assert run["state"] == "ready" and len(run["machines"]) == 2
    if cloud:
        assert run["cloud"]["owns_dashboard"] is True and run["cloud"]["url"].startswith("http://localhost:")
        dashboard_run = Path(run["cloud"]["dashboard"]) / ".verify" / "run"
        assert (dashboard_run / "inngest.pid").exists(), "Pairing did not start Inngest"
    print(json.dumps(dict(daemon=daemon, startup_seconds=run["timings"]["total"],
                          observed_overlap_seconds=round(overlap, 3), manifest=str(manifest))))
finally:
    subprocess.run([helper, "down", manifest], check=True)

assert json.loads(manifest.read_text())["state"] == "removed"
assert not (manifest.parent / "private").exists(), "Private credentials survived cleanup"
if cloud:
    assert not dashboard_run.exists(), "down left the dashboard it started running"
