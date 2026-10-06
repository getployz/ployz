#!/usr/bin/env python3
"""Check verify-cluster's faults on real Machines: Postgres on a managed Volume keeps every acknowledged write
across a power cut of its Machine, and with --cloud a Deploy whose runner is killed completes once it restarts.

Usage: test-verify-faults.py [daemon] [--cloud]. Prints the probe's report, including the longest gap between
acknowledged writes."""

import json
from pathlib import Path
import signal
import subprocess
import sys
import time

core = Path(__file__).resolve().parents[1]
helper = core / "scripts" / "verify-cluster"
cloud = "--cloud" in sys.argv[1:]
positional = [arg for arg in sys.argv[1:] if arg != "--cloud"]
daemon = positional[0] if positional else "stable"
result = subprocess.run([helper, "up", "--machines", "2", "--daemon", daemon, *(["--cloud"] if cloud else [])],
                        cwd=core, stdout=subprocess.PIPE, text=True, check=True)
manifest = Path(result.stdout.strip())
evidence = manifest.parent / "evidence"


def verify(*args, **kwargs):
    return subprocess.run([helper, *args], check=True, stdout=subprocess.PIPE, text=True, **kwargs).stdout


def cli(*args):
    return verify("cli", "--timeout", "600", manifest, "--", *args)


def acks():
    logs = sorted(evidence.glob("ackprobe-db-*.jsonl"))
    return [line for line in logs[-1].read_text().splitlines() if '"ack"' in line] if logs else []


def wait(condition, seconds, message):
    deadline = time.monotonic() + seconds
    while not condition():
        assert time.monotonic() < deadline, message
        time.sleep(0.5)


probe = None
try:
    run = json.loads(manifest.read_text())
    cli("project", "new", "verify")
    cli("link", "--project", "verify")
    cli("service", "add", "db", "--image", "postgres:16-alpine")
    cli("set", "db.env.POSTGRES_PASSWORD=verify", "db.env.PGDATA=/var/lib/postgresql/data/pgdata")
    cli("volume", "add", "data", "--size", "2GB", "--mount", "db:/var/lib/postgresql/data")
    cli("deploy")
    containers = json.loads(cli("--json", "ps"))["containers"]
    writer = next(node["name"] for node in run["machines"]
                  for container in containers if container["labels"].get("ployz.service.name") == "db"
                  and container["machine_id"] == node["machine_id"])

    probe = subprocess.Popen([helper, "probe", manifest, "db"], stdout=subprocess.PIPE, text=True)
    wait(lambda: len(acks()) >= 400, 300, "The probe never reached 400 acknowledged writes")
    verify("power", "off", manifest, writer)
    time.sleep(10)
    verify("power", "on", manifest, writer)
    resumed = len(acks())
    wait(lambda: len(acks()) >= resumed + 400, 300, "Writes did not resume after power on")
    probe.send_signal(signal.SIGINT)
    output, _ = probe.communicate(timeout=600)
    report = json.loads(output)
    assert probe.returncode == 0 and report["lost"] == [], f"Acknowledged writes were lost: {report}"
    assert report["present"] >= report["acked"] > 800, report
    assert report["longest_gap_seconds"] >= 10, f"The longest gap is shorter than the power cut: {report}"

    if cloud:
        def newest():
            return json.loads(cli("--json", "deployment", "ls"))["deployments"][0]

        number = newest()["number"] + 1
        cli("service", "add", "web", "--image", "nginx:1.29-alpine")
        deploy = subprocess.Popen([helper, "cli", "--timeout", "900", manifest, "--", "deploy"],
                                  stdout=subprocess.PIPE, text=True)
        wait(lambda: newest()["number"] == number and newest()["status"] == "running", 120, "The Deploy never started running")
        verify("runner", "kill", manifest)
        time.sleep(10)
        assert deploy.poll() is None and newest()["status"] == "running", f"The Deploy settled without its runner: {newest()}"
        verify("runner", "start", manifest)
        output = deploy.communicate(timeout=900)[0]
        assert deploy.returncode == 0 and "of verify/production: applied" in output, f"The Deploy did not complete:\n{output}"
        web = [container for container in json.loads(cli("--json", "ps"))["containers"]
               if container["labels"].get("ployz.service.name") == "web"]
        assert len(web) == 1, f"Expected one web container after the resumed Deploy: {web}"
    print(json.dumps(dict(daemon=daemon, writer=writer, cloud=cloud, **report)))
finally:
    if probe and probe.poll() is None:
        probe.kill()
    subprocess.run([helper, "down", manifest], check=True)
