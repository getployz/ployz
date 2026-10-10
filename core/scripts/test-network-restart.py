#!/usr/bin/env python3
"""Check retained WireGuard peers while restarting a Machine in a verify cluster."""

import argparse
import json
import pathlib
import subprocess
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=pathlib.Path)
    parser.add_argument("--machine", default="machine-1")
    args = parser.parse_args()
    manifest = args.manifest.resolve()
    core = pathlib.Path(__file__).resolve().parents[1]

    def shell(script):
        return subprocess.run(
            [
                str(core / "scripts/verify-cluster"), "exec", str(manifest),
                args.machine, "--", "bash", "-ec", script,
            ],
            cwd=core, text=True, capture_output=True, check=True,
        ).stdout.strip()

    before = shell("wg show ployz-wg peers").splitlines()
    assert before, "Need a live peer before probing restart"
    probe = f"/tmp/ployz-network-restart-{uuid.uuid4().hex}"
    evidence = manifest.parent / "evidence"
    evidence.mkdir(exist_ok=True)
    trace_path = evidence / f"{pathlib.Path(probe).name}.trace"
    shell(
        f"setsid bash -c 'while [ ! -f {probe}.stop ]; do "
        "date +%s.%N; wg show ployz-wg peers; echo SAMPLE_END; sleep 0.05; "
        f"done' >{probe}.trace 2>&1 </dev/null &"
    )
    try:
        shell("systemctl restart ployz")
    finally:
        shell(f"touch {probe}.stop")
        time.sleep(0.2)
        trace = shell(f"cat {probe}.trace")
        trace_path.write_text(trace)

    samples = [sample.strip().splitlines() for sample in trace.split("SAMPLE_END")[:-1]]
    missing = [sample for sample in samples if not set(before).issubset(sample[1:])]
    report = {
        "samples": len(samples), "missing_peer_samples": len(missing),
        "first_missing": missing[:1], "trace": str(trace_path),
    }
    trace_path.with_suffix(".json").write_text(json.dumps(report) + "\n")
    print(json.dumps(report))
    assert samples, "Restart probe did not produce any complete samples"
    assert not missing, "Daemon restart temporarily removes an installed peer"


if __name__ == "__main__":
    main()
