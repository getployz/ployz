#!/usr/bin/env python3
"""Prove Warm Move end to end on real Machines paired with Cloud: Restore, removal, demotion, deletion,
stale SetVolumeCopy replays, deploys held mid-Move, Release, mirror reuse and slow or failing starts.

  python3 scripts/test-verify-warm-move.py --list
  python3 scripts/test-verify-warm-move.py [--manifest M] [check ...]

Without --manifest the script boots its own run with the Machines the selected checks need and takes it
down afterwards. It builds with the verify fault hooks, so a --manifest run must come from
  PLOYZ_VERIFY_CARGO_ARGS='--features ployz/verify-faults,ployzd/verify-faults' \\
    scripts/verify-cluster up --machines 2 --daemon checkout --cloud
on a fresh run: setup creates the project and one Volume per check. Writers land on machine-1, and
machine-2 is the Server each removal check takes out and adds back, so the removal checks run first.
Each check prints PASS or FAIL lines and writes evidence to the run's evidence/warm-move-<time>/.
The exit is nonzero on any FAIL.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.request

sys.dont_write_bytecode = True

CORE = Path(__file__).resolve().parents[1]
HELPER = CORE / "scripts" / "verify-cluster"
FAULT_BUILD = "--features ployz/verify-faults,ployzd/verify-faults"

A, B = "machine-1", "machine-2"
POOL = "ployz"
IMAGE = "postgres:16-alpine"
TERMINAL = {"done", "failed", "cancelled", "lost", "not_started"}
STOPPED = {"cancelled", "lost"}
DOOMED = "doomed"


class CheckError(Exception):
    pass


def parse_json(text):
    text = (text or "").strip()
    for start in [0] + [index for index, char in enumerate(text) if char in "{["][::-1]:
        try:
            return json.loads(text[start:])
        except json.JSONDecodeError:
            continue
    return None


def find_key(data, key):
    if isinstance(data, dict):
        if key in data:
            return data[key]
        values = data.values()
    elif isinstance(data, list):
        values = data
    else:
        return None
    for value in values:
        found = find_key(value, key)
        if found is not None:
            return found
    return None


def run_rows(data):
    rows = []

    def walk(node):
        if isinstance(node, dict):
            if {"state", "kind", "id"} <= node.keys():
                rows.append(node)
                return
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for value in node:
                walk(value)
    walk(data)
    return rows


def say(data):
    return json.dumps(data)


def done(row):
    return row is not None and row.get("state") == "done"


def refused(code, row):
    return (row is None and code != 0) or (row is not None and row.get("state") in TERMINAL - {"done"})


def text_of(row, data):
    return say(row if row is not None else data)


def http(method, url, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    headers = {"content-type": "application/json"} | ({"authorization": f"Bearer {token}"} if token else {})
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read().decode()[:600]
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()[:600]
    except OSError as error:
        return 0, str(error)


class Cluster:
    def __init__(self, manifest):
        self.manifest = Path(manifest)
        self.run = json.loads(self.manifest.read_text())
        self.address = {node["name"]: node["address"] for node in self.run["machines"]}
        self.machines = [node["name"] for node in self.run["machines"]]
        self.out = self.manifest.parent / "evidence" / f"warm-move-{time.strftime('%Y%m%dT%H%M%S')}"
        self.out.mkdir(parents=True, exist_ok=True)
        self.log = (self.out / "commands.log").open("a")
        self.physicals = {}
        self.touch = iter(range(1, 10_000))
        self.check = "setup"
        self.verdicts = []

    def command(self, *args, timeout=300):
        command = [str(HELPER), *map(str, args)]
        try:
            result = subprocess.run(command, capture_output=True, text=True, timeout=timeout + 60)
        except subprocess.TimeoutExpired as error:
            raise CheckError(f"timed out after {timeout}s: {command[1:8]}") from error
        self.log.write(say(dict(check=self.check, command=command[1:], exit=result.returncode,
                                stdout=result.stdout[-1500:], stderr=result.stderr[-800:])) + "\n")
        self.log.flush()
        return result

    def sh(self, machine, script, timeout=300, required=True):
        result = self.command("exec", "--timeout", timeout, self.manifest, machine, "--", "bash", "-ec", script,
                              timeout=timeout)
        if required and result.returncode:
            raise CheckError(f"{machine}: {script[:200]!r} exit {result.returncode}: "
                             f"{result.stdout[-600:]} {result.stderr[-600:]}")
        return result

    def cli(self, *args, timeout=300, required=False):
        result = self.command("cli", "--timeout", timeout, self.manifest, "--", "--json", *args, timeout=timeout)
        data = parse_json(result.stdout) or parse_json(result.stderr) or dict(
            raw=(result.stdout + " " + result.stderr).strip()[-1500:])
        if required and result.returncode:
            raise CheckError(f"ployz {' '.join(map(str, args))[:200]} exit {result.returncode}: {say(data)[:800]}")
        return result.returncode, data

    def power(self, state, machine):
        self.command("power", state, self.manifest, machine, timeout=600)

    def partition(self, state, machine):
        self.command("partition", state, self.manifest, machine, timeout=180)

    def runner(self, state):
        result = self.command("runner", state, self.manifest, timeout=180)
        if result.returncode:
            raise CheckError(f"runner {state}: {result.stdout[-400:]} {result.stderr[-400:]}")

    # Machine state

    def physical(self, volume, env=None):
        if (volume, env) not in self.physicals:
            _, inspect = self.cli("volume", "inspect", volume, *env_flag(env), required=True)
            environment = inspect["environment"]
            self.physicals[(volume, env)] = f"{environment['project']}-{environment['name']}_vol-{inspect['id']}"
        return self.physicals[(volume, env)]

    def prop(self, machine, dataset, key):
        return self.sh(machine, f"zfs get -H -o value {shlex.quote(key)} {shlex.quote(dataset)} 2>/dev/null "
                                "|| echo absent").stdout.strip()

    def exists(self, machine, dataset):
        return self.sh(machine, f"zfs list -H -o name {shlex.quote(dataset)} >/dev/null 2>&1 && echo yes "
                                "|| echo no").stdout.strip() == "yes"

    def snapshots(self, machine, dataset):
        listing = self.sh(machine, f"zfs list -H -t snapshot -o name -d 1 {shlex.quote(dataset)} 2>/dev/null "
                                   "|| true").stdout
        return [line.strip() for line in listing.splitlines() if line.strip()]

    def containers(self, machine, phys, every=False):
        flag = "-aq" if every else "-q"
        listing = self.sh(machine, f"/usr/bin/docker ps {flag} --no-trunc --filter volume={shlex.quote(phys)}").stdout
        return [line for line in listing.splitlines() if line.strip()]

    def live(self, phys):
        return {machine: self.containers(machine, phys) for machine in self.machines}

    def psql(self, machine, phys, sql, required=True):
        found = self.containers(machine, phys)
        if not found:
            raise CheckError(f"no running container for {phys} on {machine}")
        return self.sh(machine, f"/usr/bin/docker exec {found[0]} psql -U postgres -Atc {shlex.quote(sql)}",
                       required=required).stdout.strip()

    def ready(self, machine, phys, limit=120):
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            found = self.containers(machine, phys)
            if found and not self.sh(machine, f"/usr/bin/docker exec {found[0]} pg_isready -U postgres",
                                     required=False).returncode:
                return True
            time.sleep(3)
        return False

    def seed(self, machine, phys, value):
        self.psql(machine, phys, "create table if not exists kept (n int)")
        self.psql(machine, phys, f"insert into kept values ({value})")

    def rows(self, machine, phys):
        if not self.containers(machine, phys):
            return "no db"
        return self.psql(machine, phys, "select coalesce(string_agg(n::text, ',' order by n), '') from kept",
                         required=False)

    def copies(self, phys):
        found = {}
        for machine in self.machines:
            for dataset in (root(phys), fs(phys)):
                value = self.prop(machine, dataset, "readonly")
                if value != "absent":
                    found[f"{machine}:{dataset}"] = value
        return found

    def writable(self, phys):
        return sorted(where for where, value in self.copies(phys).items() if value == "off")

    def inspect(self, machine, phys):
        _, reply = self.rpc(machine, "inspect_volume_copy", dict(name=phys))
        return find_key(reply, "copy")

    def rpc(self, machine, command, payload):
        return self.cli("debug", "volume-rpc", machine, say(dict(command=command, payload=payload)))

    def members(self):
        _, listed = self.cli("server", "ls")
        text = say(listed)
        return {machine for machine in self.machines if f'"{machine}"' in text}

    # Faults

    def fault(self, machine, value):
        """Arm or disarm without a restart: the daemons read /run/ployz/fault on every check."""
        if value:
            self.sh(machine, f"mkdir -p /run/ployz && printf '%s\\n' {shlex.quote(value)} > /run/ployz/fault.tmp "
                             "&& mv /run/ployz/fault.tmp /run/ployz/fault")
        else:
            self.sh(machine, "rm -f /run/ployz/fault")

    def fault_lines(self, machine, since):
        output = self.sh(machine, f"journalctl -u ployz.service --since @{int(since) - 2} --no-pager -o short-unix "
                                  "| grep -F 'fault:' || true", required=False).stdout
        lines = []
        for line in output.splitlines():
            at, _, rest = line.partition(" ")
            try:
                lines.append((float(at), rest))
            except ValueError:
                continue
        return lines

    def stopped_at(self, machine, phys, since):
        result = self.sh(machine, f"/usr/bin/docker ps -aq --filter volume={shlex.quote(phys)} --filter status=exited "
                                  "| xargs -r /usr/bin/docker inspect -f '{{.State.FinishedAt}}' "
                                  "| xargs -r -n1 date +%s.%N -d", required=False)
        times = [float(word) for word in result.stdout.split() if re.fullmatch(r"\d+\.\d+", word)]
        times = [at for at in times if at >= since]
        return min(times) if times else None

    def wait_held(self, volume, row, phys, source, target, cell, since, limit=900):
        """Wait until `target` logs that its fault delays `cell`, after the db on `source` stopped for the cut."""
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            stopped = self.stopped_at(source, phys, since)
            if stopped is not None:
                for at, line in self.fault_lines(target, since):
                    if at > stopped and "delaying switch verb" in line and cell in line:
                        return line.strip()[:300]
            if self.row(volume, row["id"]).get("state") in TERMINAL:
                return None
            time.sleep(1)
        return None

    def wrap_runc(self, machine, tag, phys, body):
        """Replace the Machine's runc with a wrapper that runs `body` before creating a container that mounts `phys`.

        ployzd starts containers through the Docker API, so the docker CLI never sees the start; runc does."""
        wrapper = ("#!/bin/sh\ncase \" $* \" in *\" create \"*)\n"
                   "  bundle=; prev=; for arg in \"$@\"; do [ \"$prev\" = --bundle ] && bundle=$arg; prev=$arg; done\n"
                   f"  if grep -q {phys} \"$bundle/config.json\" 2>/dev/null; then\n{body}\n  fi;;\nesac\n"
                   f"exec /usr/bin/runc.{tag}-original \"$@\"\n")
        self.sh(machine, f"rm -f /tmp/{tag}-start.log && cp -p /usr/bin/runc /usr/bin/runc.{tag}-original "
                         f"&& printf %s {shlex.quote(wrapper)} > /usr/bin/runc.{tag}-wrapper "
                         f"&& chmod +x /usr/bin/runc.{tag}-wrapper && mv /usr/bin/runc.{tag}-wrapper /usr/bin/runc")

    def slow_runc(self, machine, tag, phys, seconds):
        self.wrap_runc(machine, tag, phys, f"    echo \"begin $(date +%s)\" >> /tmp/{tag}-start.log\n    sleep {seconds}\n"
                                           f"    echo \"end $(date +%s)\" >> /tmp/{tag}-start.log")

    def plain_runc(self, machine, tag):
        self.sh(machine, f"if [ -f /usr/bin/runc.{tag}-original ]; then "
                         f"mv /usr/bin/runc.{tag}-original /usr/bin/runc; fi", required=False)

    def start_log(self, machine, tag):
        return self.sh(machine, f"cat /tmp/{tag}-start.log 2>/dev/null || true", required=False).stdout.strip()

    def wait_start_log(self, machine, tag, limit=600):
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            began = self.sh(machine, f"grep -m1 '^begin' /tmp/{tag}-start.log 2>/dev/null || true",
                            required=False).stdout.strip()
            if began:
                return began
            time.sleep(1)
        return None

    # Cloud runs

    def approve(self, reply):
        """Approve as the operator would in Cloud and return the approval id, or None when nothing asked."""
        error = reply.get("error") if isinstance(reply, dict) else None
        if not isinstance(error, dict) or error.get("code") != "approval_required":
            return None
        details = error["details"]
        token = (self.manifest.parent / "private" / "cloud-token").read_text().strip()
        status, text = http("POST", f"{self.run['cloud']['url']}/api/cli/approvals/{details['approval_id']}",
                            dict(approve=dict(digest=details["approval"])), token=token)
        if not 200 <= status < 300:
            raise CheckError(f"approval {details['approval_id']} refused: {status} {text[:300]}")
        return details["approval_id"]

    def accepted(self, *args, timeout=1500):
        """Run a command, approving it and following its own retry hint the way an operator would."""
        args = list(args)
        code, data = self.cli(*args, timeout=timeout)
        for _ in range(3):
            if not code:
                break
            approval = self.approve(data)
            retry = find_key(data, "retry")
            if approval:
                args = [*args, "--approval", approval]
            elif isinstance(retry, str) and retry.strip():
                words = shlex.split(retry)
                args = words[1:] if words and words[0] == "ployz" else words
            else:
                break
            code, data = self.cli(*args, timeout=timeout)
        return code, data

    def start_run(self, *args, env=None):
        code, data = self.accepted(*args, *env_flag(env))
        rows = run_rows(data) if code == 0 else []
        return code, (rows[0] if rows else None), data

    def row(self, volume, run_id, env=None):
        _, data = self.cli("volume", "runs", volume, *env_flag(env))
        return next((row for row in run_rows(data) if row.get("id") == run_id), {})

    def wait_run(self, volume, run_id, env=None, limit=1500):
        deadline = time.monotonic() + limit
        last = {}
        while time.monotonic() < deadline:
            last = self.row(volume, run_id, env) or last
            if last.get("state") in TERMINAL:
                return last
            time.sleep(4)
        return {**last, "id": run_id, "state": f"still {last.get('state', 'unseen')} after {limit}s"}

    def run_volume(self, *args, volume, env=None, limit=1500):
        code, row, data = self.start_run(*args, env=env)
        if row and row.get("state") not in TERMINAL:
            row = self.wait_run(volume, row["id"], env, limit)
        return code, row, data

    def move(self, volume, to, limit=1500):
        return self.run_volume("volume", "move", volume, "--to", to, volume=volume, limit=limit)

    def mirror(self, volume, to, env=None):
        phys = self.physical(volume, env)
        if self.exists(to, fs(phys)):
            return
        code, row, data = self.run_volume("volume", "mirror", volume, "--to", to, volume=volume, env=env)
        if not done(row) or not self.exists(to, fs(phys)):
            raise CheckError(f"mirror {volume} --to {to} did not land: {text_of(row, data)[:800]}")

    def restore(self, volume, source, limit=1800):
        return self.accepted("volume", "restore", volume, "--from", source, "--wait", timeout=limit)

    def plain_deploy(self, service):
        self.cli("set", f"{service}.env.VC_TOUCH={next(self.touch)}", required=True)
        return self.cli("deploy", timeout=900)

    def dashboard_env(self):
        dashboard = Path(self.run["cloud"]["dashboard"])
        pairs = (line.split("=", 1) for line in (dashboard / ".verify" / "run" / "env").read_text().splitlines()
                 if "=" in line)
        return dict(pairs), "ployz-verify-" + hashlib.sha1(str(dashboard.parent).encode()).hexdigest()[:10]

    def cancel(self, run_id):
        """Cancel the Inngest run behind a volume_run row; the row closes once a runner is back."""
        environment, postgres = self.dashboard_env()
        query = subprocess.run(["docker", "exec", postgres, "psql", "-U", "postgres", "-d", "ployz_cloud", "-Atc",
                                f"select inngest_run_id from volume_run where id = '{run_id}'"],
                               capture_output=True, text=True, timeout=60)
        inngest = query.stdout.strip()
        base = environment["INNGEST_BASE_URL"]
        status, text = http("DELETE", f"{base}/v1/runs/{inngest}")
        evidence = [f"DELETE {inngest} {status} {text[:200]}"]
        if status not in (200, 202, 204):
            mutation = "mutation($envID: UUID!, $runID: ULID!) { cancelRun(envID: $envID, runID: $runID) { id } }"
            for env_id in ("00000000-0000-4000-b000-000000000000", "00000000-0000-0000-0000-000000000000"):
                status, text = http("POST", f"{base}/v0/gql",
                                    dict(query=mutation, variables=dict(envID=env_id, runID=inngest)))
                evidence.append(f"gql {env_id} {status} {text[:200]}")
                if status == 200 and '"errors"' not in text:
                    break
        return evidence

    def next_run(self, volume, kind, earlier, limit=180):
        """The first `kind` run on `volume` created after the row `earlier`, as soon as it shows."""
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            _, data = self.cli("volume", "runs", volume)
            later = [row for row in run_rows(data) if row.get("kind") == kind and row.get("id") != earlier.get("id")
                     and row.get("created_at", "") > earlier.get("created_at", "")]
            if later:
                return min(later, key=lambda row: row["created_at"])
            time.sleep(4)
        return None

    def stop_move(self, volume, phys, source, target):
        """Hold a Move to `target` in its last pull after the cut, before it commits, then cancel it with the
        runner down. The Move ends warm, so Cloud starts a release back to `source`."""
        self.fault(target, "delay-rpc:SetVolumeCopy.MirrorToMirror:25")
        since = time.time()
        code, row, data = self.start_run("volume", "move", volume, "--to", target)
        if row is None:
            self.fault(target, None)
            raise CheckError(f"move refused: {say(data)[:600]}")
        held = self.wait_held(volume, row, phys, source, target, "MirrorToMirror", since)
        self.runner("kill")
        try:
            cancelled = self.cancel(row["id"])
        finally:
            self.fault(target, None)
            self.runner("start")
        stopped = self.wait_run(volume, row["id"], limit=600)
        return dict(held=held, cancel=cancelled, run=stopped)

    # Removal and rejoin

    def remove_dead(self, machine, limit=900):
        """`server forget --confirm dead`, retried while the entry Server still lists it as answering:
        until its handshake goes stale, the copies it holds are asked for and the ask times out."""
        deadline = time.monotonic() + limit
        attempts = []
        while True:
            code, data = self.accepted("server", "forget", machine, "--confirm", "dead", timeout=600)
            attempts.append(f"exit {code} {say(data)[:300]}")
            if code == 0:
                return attempts
            retry = "does not match" in say(data) or find_key(data, "code") == "unavailable"
            if not retry or time.monotonic() > deadline:
                raise CheckError(f"server forget {machine} --confirm dead refused: {attempts[-3:]}")
            time.sleep(10)

    def daemon_up(self, machine):
        self.sh(machine, "for i in $(seq 180); do systemctl is-active --quiet ployz.service && exit 0; sleep 1; done; "
                         "exit 1", timeout=240)

    def rejoin(self, machine):
        """Add a removed Machine back the way verify-cluster up enrolled it, then let it take Services."""
        if machine in self.members():
            return "still a member"
        started = time.time()
        key = self.manifest.parent / "private" / "id_ed25519"
        code, data = self.cli("server", "add", "root@" + self.address[machine], "--no-install", "--storage", "none",
                              "--yes", "--name", machine, "--ssh-key", key, "--public-ip", "none",
                              "--wg-endpoint", self.address[machine] + ":51820", "--wg-mtu", "1420", timeout=900)
        joined_partly = code and "Server joined" in say(data)
        if code and not joined_partly:
            raise CheckError(f"server add {machine} after its removal failed: {say(data)[:800]}")
        self.accept_services(machine)
        if not joined_partly:
            return f"added back in {round(time.time() - started)}s"
        deadline = time.time() + 600
        while True:
            code, deployed = self.cli("deploy", timeout=900)
            if code == 0:
                return f"joined with catch-up incomplete; the redeploy it asks for finished {round(time.time() - started)}s later"
            if time.time() > deadline:
                raise CheckError(f"{machine} joined but redeploy still fails after 600s: {say(deployed)[:800]}")
            time.sleep(30)

    def accept_services(self, machine):
        """The entry Server may not route to a fresh add over the mesh for some seconds; retry as a user would."""
        deadline = time.time() + 120
        while True:
            code, data = self.cli("server", "set", machine, "--accepts-services=true")
            if code == 0:
                return
            if find_key(data, "code") != "unavailable" or time.time() > deadline:
                raise CheckError(f"server set {machine} --accepts-services=true after its add failed: {say(data)[:800]}")
            time.sleep(5)

    def wait_gone(self, phys, limit=480):
        started = time.monotonic()
        while time.monotonic() - started < limit:
            if not any(self.exists(machine, slot(phys)) for machine in self.machines):
                return round(time.monotonic() - started)
            time.sleep(10)
        return None

    # Verdicts and evidence

    def verdict(self, name, ok, evidence):
        self.verdicts.append((name, ok))
        path = self.out / f"{self.check}.txt"
        with path.open("a") as notes:
            notes.write(f"{'PASS' if ok else 'FAIL'} {name}: {evidence}\n")
        print(f"PASS {name} ({path})" if ok else f"FAIL {name}: {evidence[:1500]} ({path})", flush=True)

    def evidence(self, since, volumes):
        for machine in self.machines:
            listing = self.sh(machine, f"zfs list -t all -H -o name,used,refer,readonly -r {POOL} 2>/dev/null || true",
                              required=False).stdout
            (self.out / f"{self.check}-{machine}.zfs").write_text(listing)
            journal = self.sh(machine, f"journalctl -u ployz.service -u ployz-volume-plugin.service --since @{int(since) - 2} "
                                       "--no-pager -o short-iso", required=False, timeout=120).stdout
            (self.out / f"{self.check}-{machine}.journal").write_text(journal)
        runs = {}
        for volume, env in volumes:
            _, data = self.cli("volume", "runs", volume, *env_flag(env))
            runs[volume] = run_rows(data) or data
        (self.out / f"{self.check}-runs.json").write_text(json.dumps(runs, indent=2) + "\n")


class Recorder:
    """Samples, every ~0.1 s on each Machine, the root and slot fs readonly flags and the running containers,
    so the merged timeline counts writable copies and running writers at every instant."""

    def __init__(self, cluster, tag, phys):
        self.cluster, self.tag, self.phys = cluster, tag, phys

    def __enter__(self):
        loop = (f"while :; do t=$(date +%s.%N); "
                f"d=$(zfs list -H -o name,readonly {root(self.phys)} {fs(self.phys)} 2>/dev/null "
                "| awk '{printf \"%s=%s,\", $1, $2}'); "
                f"c=$(/usr/bin/docker ps -q --filter volume={self.phys} 2>/dev/null); "
                "echo \"$t|$d|$(echo \"$c\" | grep -c .)\"; sleep 0.1; done")
        for machine in self.cluster.machines:
            self.cluster.sh(machine, f"rm -f /tmp/rec-{self.tag}.log; nohup setsid bash -c {shlex.quote(loop)} "
                                     f"> /tmp/rec-{self.tag}.log 2>&1 < /dev/null & echo $! > /tmp/rec-{self.tag}.pid")
        time.sleep(2)
        for machine in self.cluster.machines:
            alive = self.cluster.sh(machine, f"kill -0 $(cat /tmp/rec-{self.tag}.pid) && wc -l < /tmp/rec-{self.tag}.log",
                                    required=False)
            if alive.returncode:
                raise CheckError(f"recorder on {machine} did not survive: {alive.stdout} {alive.stderr}")
        return self

    def __exit__(self, *_):
        for machine in self.cluster.machines:
            self.cluster.sh(machine, f"kill $(cat /tmp/rec-{self.tag}.pid) 2>/dev/null || true", required=False)

    def timeline(self):
        events = []
        for machine in self.cluster.machines:
            text = self.cluster.sh(machine, f"cat /tmp/rec-{self.tag}.log", timeout=300).stdout
            for line in text.splitlines():
                parts = line.split("|")
                if len(parts) != 3:
                    continue
                try:
                    at, running = float(parts[0]), int(parts[2].strip() or 0)
                except ValueError:
                    continue
                datasets = dict(item.split("=", 1) for item in parts[1].split(",") if "=" in item)
                events.append((at, machine, datasets, running))
        events.sort(key=lambda event: event[0])
        latest = {}
        worst = dict(writable=(0, None), running=(0, None))
        for at, machine, datasets, running in events:
            latest[machine] = (datasets, running)
            writable = [f"{m}:{name}" for m, (ds, _) in latest.items() for name, ro in ds.items() if ro == "off"]
            live = sum(count for _, count in latest.values())
            for key, value, where in (("writable", len(writable), writable), ("running", live, None)):
                if value > worst[key][0]:
                    worst[key] = (value, f"{at:.2f} {where or {m: v[1] for m, v in latest.items()}}")
        return dict(worst, samples=len(events))


def root(phys):
    return f"{POOL}/ployz/{phys}"


def slot(phys):
    return f"{POOL}/ployz-mirror/{phys}"


def fs(phys):
    return f"{slot(phys)}/fs"


def env_flag(env):
    return ["--env", env] if env else []


def other(machine):
    return B if machine == A else A


def judged(vc, prefix, checks, evidence):
    failed = [label for label, ok in checks.items() if not ok]
    vc.verdict(prefix, not failed, f"failed {failed}; {evidence}")


# Restore only as the only holder

def restore_only_holder(vc):
    phys = vc.physical("india")
    vc.seed(A, phys, 1)
    _, moved, data = vc.move("india", B)
    if not done(moved) or not vc.ready(B, phys):
        raise CheckError(f"move india to {B}: {text_of(moved, data)[:600]}")
    vc.psql(B, phys, "insert into kept values (2)")
    _, synced, data = vc.run_volume("volume", "sync", "india", volume="india")
    if not done(synced):
        raise CheckError(f"sync india: {text_of(synced, data)[:600]}")
    vc.psql(B, phys, "insert into kept values (3)")
    refusals = {}
    for source, phrase in ((A, f"also has copies on {B}"), (B, f"writer is already on {B}")):
        code, row, data = vc.run_volume("volume", "restore", "india", "--from", source, volume="india")
        refusals[source] = (refused(code, row) and phrase in text_of(row, data), text_of(row, data)[:400])
    code_rm, removed = vc.accepted("server", "rm", B, "--confirm", B)
    code_r, restored = vc.restore("india", A)
    code_d, _ = vc.plain_deploy("indiadb")
    rows = vc.rows(A, phys) if vc.ready(A, phys, limit=300) else "no db"
    writable = vc.writable(phys)
    judged(vc, "C9", {
        "restore refused while another holder answers": all(ok for ok, _ in refusals.values()),
        f"server rm {B} with reset succeeded": code_rm == 0,
        "restore reports the loss since the newest snapshot":
            code_r == 0 and f"restored on {A} from" in say(restored) and "writes after that time are lost" in say(restored),
        "rows up to the last sync survive and the later write is lost": rows == "1,2",
        f"exactly one writable copy, on {A}": len(writable) == 1 and writable[0].startswith(A + ":"),
    }, f"refusals {say(refusals)[:1200]}; rm exit {code_rm} {say(removed)[:300]}; restore exit {code_r} "
       f"{say(restored)[:500]}; deploy exit {code_d}; rows on {A} {rows!r}; writable {writable}")
    rejoin_verdict(vc, "C9")


def rejoin_verdict(vc, name):
    try:
        vc.verdict(f"{name}_rejoin", True, vc.rejoin(B))
    except CheckError as error:
        vc.verdict(f"{name}_rejoin", False, str(error))


# Removed after the target accepted the Move, then added back

def rm_target_then_readd(vc):
    phys = vc.physical("juliet")
    vc.seed(A, phys, 1)
    vc.fault(B, "delay-rpc:SetVolumeCopy.MirrorToWriter:900")
    since = time.time()
    _, row, data = vc.start_run("volume", "move", "juliet", "--to", B)
    if row is None:
        vc.fault(B, None)
        raise CheckError(f"move refused: {say(data)[:600]}")
    held = vc.wait_held("juliet", row, phys, A, B, "MirrorToWriter", since)
    stale_txg = vc.prop(B, fs(phys), "createtxg")
    vc.power("off", B)
    try:
        first = vc.wait_run("juliet", row["id"], limit=1800)
        attempts = vc.remove_dead(B)
        code_r, restored = vc.restore("juliet", A)
        code_d, _ = vc.plain_deploy("julietdb")
        if vc.ready(A, phys, limit=300):
            vc.psql(A, phys, "insert into kept values (2)")
    finally:
        vc.power("on", B)
    vc.daemon_up(B)
    vc.fault(B, None)
    joined = vc.rejoin(B)
    _, again, data = vc.move("juliet", B)
    rows = vc.rows(B, phys) if vc.ready(B, phys, limit=300) else "no db"
    writable = vc.writable(phys)
    judged(vc, "rm_target_after_accept_then_readd", {
        f"{B} held MirrorToWriter after the cut": bool(held),
        "first move did not finish before the removal": first.get("state") in TERMINAL - {"done"},
        f"restore on {A} succeeded": code_r == 0,
        f"{B} added back without a reset": bool(joined),
        f"the next move lands on {B} with both rows": done(again) and rows == "1,2",
        f"exactly one writable copy, on {B}": len(writable) == 1 and writable[0].startswith(B + ":"),
    }, f"held {held}; stale slot txg {stale_txg}, root txg after {vc.prop(B, root(phys), 'createtxg')}; "
       f"first move {say({k: first.get(k) for k in ('state', 'step', 'message')})}; removal {attempts[-1:]}; "
       f"restore exit {code_r} {say(restored)[:300]}; deploy exit {code_d}; rejoin {joined}; "
       f"next move {text_of(again, data)[:300]}; rows on {B} {rows!r}; writable {writable}")
    _, back, data = vc.move("juliet", A)
    vc.verdict("rm_target_after_accept_then_readd_back_on_machine_1", done(back) and vc.ready(A, phys),
               f"later checks remove {B}, so juliet writes on {A} again: {text_of(back, data)[:400]}")


# A returning old copy is demoted

WRITER_PID = "/run/warm-move-kilo-writer.pid"


def returning_old_copy_demoted(vc):
    phys = vc.physical("kilo")
    vc.seed(A, phys, 1)
    _, moved, data = vc.move("kilo", B)
    if not done(moved) or not vc.ready(B, phys):
        raise CheckError(f"move kilo to {B}: {text_of(moved, data)[:600]}")
    vc.psql(B, phys, "insert into kept values (2)")
    _, synced, data = vc.run_volume("volume", "sync", "kilo", volume="kilo")
    if not done(synced):
        raise CheckError(f"sync kilo: {text_of(synced, data)[:600]}")
    container = vc.containers(B, phys)[0]
    loop = (f"i=1000; while :; do /usr/bin/docker exec {container} psql -U postgres -Atc \"insert into kept values ($i)\" "
            ">/dev/null 2>&1 && i=$((i+1)); sleep 1; done")
    vc.sh(B, f"nohup setsid bash -c {shlex.quote(loop)} >/dev/null 2>&1 < /dev/null & echo $! > {WRITER_PID}")
    try:
        vc.partition("on", B)
        try:
            attempts = vc.remove_dead(B)
            code_r, restored = vc.restore("kilo", A)
            code_d, _ = vc.plain_deploy("kilodb")
            if vc.ready(A, phys, limit=300):
                vc.psql(A, phys, "insert into kept values (3)")
        finally:
            vc.partition("off", B)
        returned = [where for where in vc.writable(phys) if where.startswith(B + ":")]
        joined = vc.rejoin(B)
        lost = vc.psql(B, phys, "select count(*) from kept where n >= 1000", required=False) \
            if vc.containers(B, phys) else "no db"
        code_s, demoted, data_s = vc.run_volume("volume", "sync", "kilo", volume="kilo")
    finally:
        vc.sh(B, f"test -f {WRITER_PID} && kill \"$(cat {WRITER_PID})\"; rm -f {WRITER_PID}", required=False)
    demote = text_of(demoted, data_s)
    b_copies = {where: value for where, value in vc.copies(phys).items() if where.startswith(B + ":")}
    writable = vc.writable(phys)
    rows = vc.rows(A, phys)
    judged(vc, "returning_old_copy_demoted", {
        f"restore on {A} succeeded": code_r == 0,
        f"{B} returned with a writable root": bool(returned),
        f"{B} added back": bool(joined),
        "the next run fails asking for a rerun":
            refused(code_s, demoted) and "is now read-only" in demote and "Run this again" in demote,
        f"{B} copy kept and read-only": bool(b_copies) and all(value == "on" for value in b_copies.values()),
        f"exactly one writable copy, the root on {A}": writable == [f"{A}:{root(phys)}"],
        f"rows on {A} intact": rows == "1,2,3",
    }, f"removal {attempts[-1:]}; restore exit {code_r} {say(restored)[:300]}; deploy exit {code_d}; "
       f"writable on return {returned}; rejoin {joined}; rows {B} wrote after its removal, lost by design: {lost}; "
       f"demote run {demote[:500]}; {B} copies {b_copies}; writable {writable}; rows on {A} {rows!r}")


# No reset of an answering holder

def no_reset_refused(vc):
    phys = vc.physical("lima")
    vc.mirror("lima", B)

    def state():
        return dict(members=sorted(vc.members()), txg=vc.prop(B, fs(phys), "createtxg"),
                    readonly=vc.prop(B, fs(phys), "readonly"))
    before = state()
    attempts = {}
    for confirm in ("dead", B):
        code, data = vc.cli("server", "forget", B, "--confirm", confirm, timeout=600)
        attempts[confirm] = (code, say(data)[:600])
    after = state()
    named = attempts[B][1]
    judged(vc, "no_reset_of_answering_writer_refused", {
        "--confirm dead refused": attempts["dead"][0] != 0,
        f"--confirm {B} refused": attempts[B][0] != 0,
        "refusal says the Server answers and holds copies": "answers" in named and "holds copies" in named and "lima" in named,
        f"{B} still a member": B in after["members"],
        f"{B} copy intact": after == before and before["txg"] != "absent" and before["readonly"] == "on",
    }, f"attempts {say(attempts)}; before {before}; after {after}")


# Deleting a mirrored Volume or Environment leaves no copy

def deletion_verdicts(vc, prefix, phys, waited):
    roots = {machine: vc.exists(machine, root(phys)) for machine in vc.machines}
    slots = {machine: vc.exists(machine, slot(phys)) for machine in vc.machines}
    vc.verdict(f"{prefix}_no_root", not any(roots.values()), f"roots {roots}")
    vc.verdict(f"{prefix}_no_slot", not any(slots.values()), f"slot parents {slots}; settled in {waited}s")


def delete_mirrored_volume(vc):
    phys = vc.physical("hotel")
    vc.mirror("hotel", B)
    vc.cli("volume", "rm", "hotel", required=True)
    code, data = vc.accepted("deploy")
    if code:
        raise CheckError(f"accepted deploy failed: {say(data)[:800]}")
    deletion_verdicts(vc, "delete_mirrored_volume", phys, vc.wait_gone(phys))


def delete_mirrored_environment(vc):
    phys = vc.physical("foxtrot", DOOMED)
    vc.mirror("foxtrot", B, DOOMED)
    code, data = vc.accepted("env", "rm", DOOMED, "--confirm", f"verify/{DOOMED}")
    removal = find_key(data, "removal")
    gone = f'"{DOOMED}"' not in say(vc.cli("env", "ls")[1])
    vc.verdict("delete_mirrored_environment_reports_success", code == 0 and removal == "removed" and gone,
               f"env rm exit {code}, removal {removal!r}, waiting_on {find_key(data, 'waiting_on')!r}; "
               f"environment gone from env ls right after={gone}; {say(data)[:800]}")
    deletion_verdicts(vc, "delete_mirrored_environment", phys, vc.wait_gone(phys))


# A stale SetVolumeCopy replay changes nothing. It carries an old gen, so the floor refuses it

def stale_replay_refused(vc):
    phys = vc.physical("delta")
    e0 = (vc.inspect(A, phys) or {}).get("epoch")
    _, moved, data = vc.move("delta", B)
    ready = vc.ready(B, phys)
    after_first = vc.inspect(B, phys) or {}
    vc.verdict("first_move_completes", done(moved) and ready and after_first.get("kind") == "root",
               f"run {text_of(moved, data)[:400]}; {B} copy {say(after_first)[:400]}")
    _, back, data = vc.move("delta", A)
    ready = vc.ready(A, phys)
    a_copy, b_copy = vc.inspect(A, phys) or {}, vc.inspect(B, phys) or {}
    vc.verdict("second_move_completes", done(back) and ready and a_copy.get("kind") == "root",
               f"run {text_of(back, data)[:400]}; {A} copy {say(a_copy)[:400]}; {B} copy {say(b_copy)[:400]}")
    first_newest = after_first.get("newest") or {}
    replays = [
        ("cut replayed on the first writer", A, "stale",
         dict(expect=dict(kind="writer", epoch=e0),
              want=dict(kind="mirror", **{"from": None}, target=first_newest.get("name"), drain=0))),
        ("promote replayed on the first target", B, "stale",
         dict(expect=dict(kind="mirror", newest=first_newest.get("guid"), slot=b_copy.get("slot")),
              want=dict(kind="writer", at=first_newest.get("guid"), epoch=after_first.get("epoch"), start=None))),
        ("older epoch on the writer", A, "stale",
         dict(expect=dict(kind="writer", epoch=a_copy.get("epoch")),
              want=dict(kind="writer", at=(a_copy.get("newest") or {}).get("guid"), epoch=after_first.get("epoch"),
                        start=None))),
    ]
    results = []
    for label, machine, reason, request in replays:
        code, reply = vc.rpc(machine, "set_volume_copy", dict(name=phys, **request))
        results.append(dict(replay=label, on=machine, exit=code, want_reason=reason, reason=find_key(reply, "reason"),
                            reply=say(reply)[:400]))
    time.sleep(5)
    live = vc.live(phys)
    writable = vc.writable(phys)
    ok = (all(r["exit"] != 0 and r["reason"] == r["want_reason"] for r in results)
          and len(live[A]) == 1 and not live[B] and writable == [f"{A}:{root(phys)}"])
    vc.verdict("stale_set_volume_copy_refused", ok,
               f"epochs e0={e0} e1={after_first.get('epoch')} e2={a_copy.get('epoch')}; replays {say(results)}; "
               f"running {live}; writable {writable}")


# A plain deploy is refused while a Move holds the Volume

def deploy_refused_mid_move(vc):
    phys = vc.physical("charlie")
    plan = [(A, B, "delay-rpc:SetVolumeCopy.MirrorToMirror:20", "MirrorToMirror"),
            (B, A, "delay-rpc:SetVolumeCopy.MirrorToWriter:20", "MirrorToWriter"),
            (A, B, None, "start")]
    holds = []
    for source, target, value, cell in plan:
        if value:
            vc.fault(target, value)
        else:
            vc.slow_runc(target, "charlie", phys, 30)
        since = time.time()
        try:
            _, row, data = vc.start_run("volume", "move", "charlie", "--to", target)
            if row is None:
                raise CheckError(f"move refused: {say(data)[:600]}")
            held = (vc.wait_held("charlie", row, phys, source, target, cell, since) if value
                    else vc.wait_start_log(target, "charlie"))
            before = vc.containers(target, phys, every=True)
            code_d, deployed = vc.plain_deploy("charliedb")
            time.sleep(5)
            after = vc.containers(target, phys, every=True)
            final = vc.wait_run("charlie", row["id"])
        finally:
            if value:
                vc.fault(target, None)
            else:
                vc.plain_runc(target, "charlie")
        ready = vc.ready(target, phys)
        outcome = deployed.get("outcome") if isinstance(deployed, dict) else None
        reason = str(outcome["reason"]) if isinstance(outcome, dict) and outcome.get("reason") else say(deployed)[:400]
        holds.append(dict(hold=cell, to=target, held=held, deploy_exit=code_d, reason=reason, target_before=before,
                          target_after=after, move=final.get("state"), message=final.get("message"), ready=ready,
                          running=vc.live(phys)))
    ok = all(h["held"] and h["deploy_exit"] != 0 and h["target_before"] == h["target_after"] and h["move"] == "done"
             and h["ready"] for h in holds) and sum(len(v) for v in holds[-1]["running"].values()) == 1
    vc.verdict("deploy_refused_mid_move", ok,
               "MirrorToMirror and MirrorToWriter held 20 s by delay-rpc on the target after the cut, the target's "
               f"runc create held 30 s; {say(holds)}")
    reasons = [h["reason"] for h in holds]
    vc.verdict("refusal_names_authored_volume",
               all("Volume charlie" in r for r in reasons) and not any("_vol-" in r for r in reasons),
               f"refusal reasons {say(reasons)}")


# Release after a stopped Move

def settled(row):
    return row is not None and row.get("state") in TERMINAL


def release_after_cancel(vc, volume, move, limit=1500):
    """Wait for the release a cancelled Move starts, then run `volume release`, which takes that release over
    while it runs or meets it ended. Answers the cancel's release once ended, and the CLI's exit and row."""
    auto = vc.next_run(volume, "release", move)
    code, released, data = vc.run_volume("volume", "release", volume, volume=volume, limit=limit)
    if auto is not None:
        auto = vc.wait_run(volume, auto["id"], limit=limit)
    return auto, code, released, data


def release_after_stopped_move(vc):
    phys = vc.physical("bravo")
    vc.psql(A, phys, "create table if not exists marks (note text); insert into marks values ('pre-05')")
    stopped = vc.stop_move("bravo", phys, A, B)
    if stopped["run"].get("state") not in STOPPED or not stopped["held"]:
        raise CheckError(f"the Move did not stop after the cut: {say(stopped)[:1200]}")
    a_copy = vc.inspect(A, phys)
    auto, code, released, data = release_after_cancel(vc, "bravo", stopped["run"])
    ready = vc.ready(A, phys)
    marks = vc.psql(A, phys, "select note from marks", required=False) if ready else "no db"
    writable = vc.writable(phys)
    b_copy = vc.inspect(B, phys) or {}
    cut = (a_copy or {}).get("newest") or {}
    judged(vc, "release_after_stopped_move", {
        "the cancel starts a release": auto is not None,
        "no release left running": settled(auto) and (released is None or settled(released)),
        "a release ends done, the cancel's or the one run after it": done(auto) or done(released),
        f"db runs on {A} with the row from before the Move": ready and "pre-05" in marks,
        f"{A} root writable": vc.prop(A, root(phys), "readonly") == "off",
        f"{B} keeps no slot at the cut's snapshot": not vc.exists(B, slot(phys))
        or (b_copy.get("kind") == "slot" and (b_copy.get("newest") or {}).get("guid") != cut.get("guid")),
        f"exactly one writable copy, on {A}": writable == [f"{A}:{root(phys)}"],
    }, f"stopped {say(stopped)[:800]}; {A} copy when stopped {say(a_copy)[:300]}; {B} copy {say(b_copy)[:300]}; "
       f"cancel's release {say(auto)[:500]}; release exit {code} {text_of(released, data)[:500]}; "
       f"marks {marks!r}; running {vc.live(phys)}; writable {writable}")


def thaw_takes_15_seconds(vc):
    phys = vc.physical("bravo")
    source = B if vc.containers(B, phys) else A
    target = other(source)
    vc.slow_runc(source, "thaw", phys, 15)
    try:
        with Recorder(vc, "thaw", phys) as recorder:
            stopped = vc.stop_move("bravo", phys, source, target)
            if stopped["run"].get("state") not in STOPPED or not stopped["held"]:
                raise CheckError(f"the Move did not stop after the cut: {say(stopped)[:1200]}")
            auto, code, released, data = release_after_cancel(vc, "bravo", stopped["run"], limit=900)
            first = dict(auto=auto, exit=code, run=released, delay=vc.start_log(source, "thaw"))
            vc.plain_runc(source, "thaw")
            retried = None
            if not (done(auto) or done(released)):
                _, retried, data = vc.run_volume("volume", "release", "bravo", volume="bravo", limit=900)
            ready = vc.ready(source, phys)
            worst = recorder.timeline()
    finally:
        vc.plain_runc(source, "thaw")
    single = worst["writable"][0] <= 1 and worst["running"][0] <= 1
    vc.verdict("thaw_takes_15_seconds",
               (done(auto) or done(released)) and "begin" in first["delay"] and ready
               and vc.prop(source, root(phys), "readonly") == "off" and single,
               f"source {source}; first releases {say(first)[:1500]}; ready={ready}; max {worst}")
    vc.verdict("slow_thaw_retry_safe",
               (done(auto) or done(released) or done(retried)) and settled(auto) and ready and single
               and not vc.containers(target, phys),
               f"retry {text_of(retried, data)[:500]}; running {vc.live(phys)}; max {worst}")


# Mirror reuse and failing starts

def move_twice_reuses_mirror(vc):
    phys = vc.physical("golf")
    vc.mirror("golf", B)
    legs = []
    for source, target in ((A, B), (B, A)):
        before = vc.prop(target, fs(phys), "createtxg")
        _, row, data = vc.move("golf", target)
        legs.append(dict(
            to=target, state=(row or {}).get("state", f"refused {say(data)[:200]}"), message=(row or {}).get("message"),
            slot_txg_before=before, root_txg_after=vc.prop(target, root(phys), "createtxg"),
            ready=vc.ready(target, phys), source_containers=vc.containers(source, phys, every=True),
            source_mountpoint=vc.sh(source, f"/usr/bin/docker volume inspect -f '{{{{.Mountpoint}}}}' {phys} 2>/dev/null "
                                            "|| echo absent").stdout.strip(),
            source_slot=vc.exists(source, fs(phys)),
            finals=[name.split("@", 1)[1] for name in vc.snapshots(target, root(phys)) if "@f-" in name]))
    ok = all(leg["state"] == "done" and leg["ready"] and leg["slot_txg_before"] == leg["root_txg_after"]
             and leg["slot_txg_before"] != "absent" and not leg["source_containers"]
             and leg["source_mountpoint"] in ("", "absent") and leg["source_slot"] and leg["finals"] for leg in legs)
    vc.verdict("move_twice_reuses_mirror", ok, say(legs)[:2500])


def start_fails_then_move_again(vc):
    phys = vc.physical("golf")
    source, target = A, B
    vc.sh(target, "touch /tmp/golf-fail-start")
    vc.wrap_runc(target, "golf", phys, "    if [ -f /tmp/golf-fail-start ]; then\n"
                                       "      echo \"fail $(date +%s)\" >> /tmp/golf-start.log; exit 1\n    fi\n"
                                       "    echo \"pass $(date +%s)\" >> /tmp/golf-start.log")
    try:
        with Recorder(vc, "golf", phys) as recorder:
            _, first, data = vc.move("golf", target, limit=1200)
            failed_log = vc.start_log(target, "golf")
            forward = vc.next_run("golf", "release", first) if first else None
            vc.sh(target, "rm -f /tmp/golf-fail-start")
            if forward is not None:
                forward = vc.wait_run("golf", forward["id"], limit=900)
            ready = vc.ready(target, phys)
            worst = recorder.timeline()
    finally:
        vc.plain_runc(target, "golf")
        vc.sh(target, "rm -f /tmp/golf-fail-start", required=False)
    live = vc.live(phys)
    judged(vc, "start_fails_then_move_again", {
        f"the start on {target} failed": "fail " in failed_log,
        "the Move ends failed": (first or {}).get("state") == "failed",
        f"the release it starts ends done once {target} starts": done(forward),
        f"db ready on {target}": ready,
        f"one container on {target}, none on {source}": len(live[target]) == 1 and not live[source],
        "at most one writable copy and one running writer": worst["writable"][0] <= 1 and worst["running"][0] <= 1,
    }, f"first {text_of(first, data)[:700]}; release {say(forward)[:500]}; "
       f"start log {vc.start_log(target, 'golf')!r}; max {worst}; running {live}")


def start_takes_15_seconds(vc):
    phys = vc.physical("echo")
    target = A if vc.containers(B, phys) else B
    source = other(target)
    began = int(time.time())
    vc.slow_runc(target, "echo15", phys, 15)
    try:
        with Recorder(vc, "echo15", phys) as recorder:
            started = time.monotonic()
            _, row, data = vc.move("echo", target, limit=900)
            elapsed = round(time.monotonic() - started, 2)
            ready = vc.ready(target, phys)
            worst = recorder.timeline()
        delay = vc.start_log(target, "echo15")
    finally:
        vc.plain_runc(target, "echo15")
    found = vc.containers(target, phys)
    environment = json.loads(vc.sh(target, f"/usr/bin/docker inspect -f '{{{{json .Config.Env}}}}' {found[0]}").stdout) \
        if found else []
    values = dict(item.split("=", 1) for item in environment if "=" in item)
    pulls = vc.sh(target, f"/usr/bin/docker events --since {began} --until {int(time.time())} --filter type=image "
                          "--filter event=pull --format '{{json .}}'").stdout.strip()
    vc.verdict("start_preserves_real_environment_and_never_pulls",
               values.get("POSTGRES_PASSWORD") == "vc-only-password" and values.get("PGDATA") == "/data/pgdata" and not pulls,
               f"POSTGRES_PASSWORD preserved={values.get('POSTGRES_PASSWORD') == 'vc-only-password'} "
               f"PGDATA={values.get('PGDATA')} image pull events={pulls!r}")
    vc.verdict("start_takes_15_seconds",
               done(row) and ready and bool(delay) and worst["writable"][0] <= 1 and worst["running"][0] <= 1
               and not vc.containers(source, phys),
               f"elapsed {elapsed}s; delay {delay!r}; max {worst}; run {text_of(row, data)[:700]}")


def start_takes_150_seconds(vc):
    phys = vc.physical("echo")
    target = A if vc.containers(B, phys) else B
    source = other(target)
    vc.slow_runc(target, "echo150", phys, 150)
    try:
        with Recorder(vc, "echo150", phys) as recorder:
            started = time.monotonic()
            _, row, data = vc.move("echo", target, limit=1200)
            elapsed = round(time.monotonic() - started, 2)
            ready = vc.ready(target, phys)
            worst = recorder.timeline()
        delay = vc.start_log(target, "echo150")
    finally:
        vc.plain_runc(target, "echo150")
    live = vc.live(phys)
    starts = delay.count("begin ")
    vc.verdict("start_takes_150_seconds",
               done(row) and ready and elapsed >= 150 and starts == 1 and worst["writable"][0] <= 1
               and worst["running"][0] <= 1 and len(live[target]) == 1 and not live[source],
               f"one held container create expected, saw {starts}; elapsed {elapsed}s; delay {delay!r}; "
               f"run {text_of(row, data)[:600]}; max {worst}; running {live}")


def plain_deploy(vc):
    phys = vc.physical("alpha")
    code, deployed = vc.plain_deploy("alphadb")
    ready = vc.ready(A, phys)
    live = vc.live(phys)
    vc.verdict("plain_deploy", code == 0 and ready and sum(len(v) for v in live.values()) == 1,
               f"deploy exit {code} {say(deployed)[:600]}; ready on {A}={ready}; running {live}")


# name, Machines, Volumes it owns as (volume, env), body. Removal checks run first: C9 removes machine-2
# with a reset, which must find no other Volume's copy there.
CHECKS = [
    ("C9", 2, [("india", None)], restore_only_holder),
    ("rm_target_after_accept_then_readd", 2, [("juliet", None)], rm_target_then_readd),
    ("returning_old_copy_demoted", 2, [("kilo", None)], returning_old_copy_demoted),
    ("no_reset_of_answering_writer_refused", 2, [("lima", None)], no_reset_refused),
    ("delete_mirrored_volume", 2, [("hotel", None)], delete_mirrored_volume),
    ("delete_mirrored_environment", 2, [("foxtrot", DOOMED)], delete_mirrored_environment),
    ("stale_set_volume_copy_refused", 2, [("delta", None)], stale_replay_refused),
    ("deploy_refused_mid_move", 2, [("charlie", None)], deploy_refused_mid_move),
    ("release_after_stopped_move", 2, [("bravo", None)], release_after_stopped_move),
    ("thaw_takes_15_seconds", 2, [("bravo", None)], thaw_takes_15_seconds),
    ("move_twice_reuses_mirror", 2, [("golf", None)], move_twice_reuses_mirror),
    ("start_fails_then_move_again", 2, [("golf", None)], start_fails_then_move_again),
    ("start_takes_15_seconds", 2, [("echo", None)], start_takes_15_seconds),
    ("start_takes_150_seconds", 2, [("echo", None)], start_takes_150_seconds),
    ("plain_deploy", 1, [("alpha", None)], plain_deploy),
]


def setup(vc, volumes):
    two = B in vc.machines
    if two:
        vc.cli("server", "set", B, "--accepts-services=false", required=True)
    vc.cli("project", "new", "verify", required=True)
    vc.cli("link", "--project", "verify", required=True)
    by_env = {}
    for volume, env in volumes:
        by_env.setdefault(env, []).append(volume)
    for env, names in by_env.items():
        if env:
            vc.cli("env", "new", env, required=True)
        # PrepareVolumes has a fixed RPC budget for a whole deploy; batches of three fit a loaded nested VM.
        for start in range(0, len(names), 3):
            for volume in names[start:start + 3]:
                service = f"{volume}db"
                vc.cli("service", "add", service, "--image", IMAGE, *env_flag(env), required=True)
                vc.cli("set", f"{service}.env.POSTGRES_PASSWORD=vc-only-password", f"{service}.env.PGDATA=/data/pgdata",
                       *env_flag(env), required=True)
                vc.cli("volume", "add", volume, "--size", "0.5GB", "--mount", f"{service}:/data", *env_flag(env),
                       required=True)
            vc.cli("deploy", *env_flag(env), timeout=1500, required=True)
    if two:
        vc.cli("server", "set", B, "--accepts-services=true", required=True)
        vc.sh(B, f"/usr/bin/docker pull -q {IMAGE}; /usr/bin/docker volume create -d ployz -o size=1g warm-move-seed && "
                 "for i in $(seq 10); do /usr/bin/docker volume rm warm-move-seed && exit 0; sleep 1; done; exit 1",
              timeout=900)
    for volume, env in volumes:
        phys = vc.physical(volume, env)
        if not vc.ready(A, phys, limit=300):
            raise CheckError(f"{volume}db never became ready on {A}")


def run_checks(manifest, selected):
    vc = Cluster(manifest)
    volumes = list(dict.fromkeys(volume for _, _, owned, _ in selected for volume in owned))
    started = time.time()
    try:
        setup(vc, volumes)
        vc.verdict("setup", True, f"physical names {say({k[0]: v for k, v in vc.physicals.items()})}")
    except (CheckError, KeyError, TypeError) as error:
        vc.verdict("setup", False, f"{type(error).__name__}: {error}")
        vc.evidence(started, volumes)
        return vc
    for name, _, owned, body in selected:
        vc.check = name
        started = time.time()
        print(f"----- {name} {time.strftime('%H:%M:%SZ', time.gmtime())}", flush=True)
        try:
            body(vc)
        except Exception as error:  # noqa: BLE001 - a failure is this check's evidence, never the run's end
            vc.verdict(name, False, f"{type(error).__name__}: {error}\n{traceback.format_exc()[-1500:]}")
        try:
            vc.evidence(started, owned)
        except CheckError as error:
            print(f"evidence for {name}: {error}", flush=True)
        print(f"----- {name} took {round(time.time() - started)}s", flush=True)
    return vc


def boot(machines):
    env = dict(os.environ)
    if FAULT_BUILD not in env.get("PLOYZ_VERIFY_CARGO_ARGS", ""):
        env["PLOYZ_VERIFY_CARGO_ARGS"] = (env.get("PLOYZ_VERIFY_CARGO_ARGS", "") + " " + FAULT_BUILD).strip()
    result = subprocess.run([str(HELPER), "up", "--machines", str(machines), "--daemon", "checkout", "--cloud"],
                            cwd=CORE, env=env, stdout=subprocess.PIPE, text=True, timeout=7200)
    lines = result.stdout.strip().splitlines()
    manifest = Path(lines[-1]) if lines else None
    if result.returncode or not manifest or not manifest.exists():
        if manifest and manifest.exists():
            subprocess.run([str(HELPER), "down", str(manifest)], cwd=CORE, timeout=1800)
        raise CheckError(f"verify-cluster up failed with exit {result.returncode}")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("checks", nargs="*", help="check names; all when none")
    parser.add_argument("--manifest", type=Path, help="a ready run started with --cloud and the fault build")
    parser.add_argument("--list", action="store_true")
    args = parser.parse_args()
    names = [name for name, *_ in CHECKS]
    if args.list:
        for name, machines, owned, _ in CHECKS:
            print(f"{name} machines={machines} volumes={','.join(volume for volume, _ in owned)}")
        return 0
    unknown = sorted(set(args.checks) - set(names))
    if unknown:
        parser.error(f"no check named {unknown}; --list shows them")
    selected = [check for check in CHECKS if not args.checks or check[0] in args.checks]
    needed = max(machines for _, machines, _, _ in selected)
    manifest = args.manifest or boot(needed)
    try:
        run = json.loads(manifest.read_text())
        if run.get("provider") != "incus" or run.get("state") != "ready" or "cloud" not in run \
                or len(run["machines"]) < needed:
            raise CheckError(f"use a ready Incus run started with --cloud and at least {needed} Machines")
        vc = run_checks(manifest, selected)
    finally:
        if not args.manifest:
            subprocess.run([str(HELPER), "down", str(manifest)], cwd=CORE, timeout=1800)
    failed = [name for name, ok in vc.verdicts if not ok]
    print(f"SUMMARY pass={len(vc.verdicts) - len(failed)} fail={len(failed)} evidence={vc.out}", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (CheckError, OSError, subprocess.TimeoutExpired) as error:
        print(f"warm-move: {error}", file=sys.stderr)
        sys.exit(1)
