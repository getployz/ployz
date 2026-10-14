#!/usr/bin/env python3
"""Check the Volume floor on an existing two-Machine Boat or Incus vc without Cloud.

Build vc with PLOYZ_VERIFY_CARGO_ARGS='--features ployz/verify-faults,ployzd/verify-faults'.

outside: a Machine removed from the cluster refuses a SetVolumeCopy and keeps no floor for it.
stop: a Floor sent while a cut stops the Volume's holder replies only after the holder stopped.
"""

import argparse
import json
from pathlib import Path
import shlex
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True

CORE = Path(__file__).resolve().parents[1]
HELPER = CORE / "scripts" / "verify-cluster"
FLOOR_AFTER_SECS = 2
HOLDER_STOP_SECS = 5
FLOORS = """
db=/var/lib/ployz/machine.db
query='select name, floor from volume_floors'
if command -v sqlite3 >/dev/null; then sqlite3 "$db" "$query"
else python3 -c "import sqlite3, sys
for name, floor in sqlite3.connect(sys.argv[1]).execute(sys.argv[2]): print(f'{name}|{floor}')" "$db" "$query"
fi
"""


class VolumeFloors:
    def __init__(self, manifest, *, prefix=None, runner=subprocess.run):
        self.manifest = Path(manifest)
        self.run = json.loads(self.manifest.read_text())
        self.prefix = prefix or "vc-floor-" + uuid.uuid4().hex[:8]
        self.runner = runner
        self.first, self.second = self.run["machines"][:2]
        for machine in (self.first, self.second):
            machine.setdefault("ssh", "root@" + machine["address"])
            machine.setdefault("endpoint", f"[{machine['address']}]:51820" if ":" in machine["address"]
                               else machine["address"] + ":51820")

    def command(self, *args, check=True, timeout=600):
        command = [str(HELPER), *map(str, args)]
        result = self.runner(command, capture_output=True, text=True, timeout=timeout + 30)
        print(json.dumps(dict(command=command[1:], exit_code=result.returncode,
                              stdout=result.stdout[-2000:], stderr=result.stderr[-1000:])), flush=True)
        if check and result.returncode:
            raise RuntimeError(f"Command failed: {command}\n{result.stdout}\n{result.stderr}")
        return result

    def guest(self, machine, script, check=True, timeout=600):
        return self.command("exec", "--timeout", timeout, self.manifest, machine["name"], "--", "bash", "-ec", script,
                            check=check, timeout=timeout)

    def cli(self, *args, check=True, timeout=600):
        result = self.command("cli", "--timeout", timeout, self.manifest, "--", "--json", *args, check=check,
                              timeout=timeout)
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError:
            return {}

    def local_rpc(self, machine, command, payload, check=True):
        request = shlex.quote(json.dumps(dict(command=command, payload=payload)))
        return self.guest(machine, f"ployz --json debug local-rpc {request}", check=check)

    def floors(self, machine):
        rows = self.guest(machine, FLOORS).stdout.split()
        return {name: int(floor) for name, floor in (row.split("|") for row in rows)}

    def daemon_pid(self, machine):
        return self.guest(machine, "systemctl show -p MainPID --value ployz.service").stdout.strip()

    def restarted(self, machine, before, limit=180):
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            now = self.daemon_pid(machine)
            if now not in ("", "0", before) and not self.guest(
                    machine, "systemctl is-active --quiet ployz.service", check=False).returncode:
                return now
            time.sleep(1)
        raise RuntimeError(f"{machine['name']}: the daemon did not restart after its reset")

    def rejoin(self, machine):
        key = self.manifest.parent / "private" / "id_ed25519"
        result = self.command("cli", "--timeout", 900, self.manifest, "--", "--json", "server", "add", machine["ssh"],
                              "--standalone", "--no-install", "--storage", "none", "--yes", "--name", machine["name"],
                              "--ssh-key", key, "--public-ip", "none", "--wg-endpoint", machine["endpoint"],
                              "--wg-mtu", "1420", check=False, timeout=900)
        follow_up = (json.loads(result.stdout) if result.stdout.strip() else {}).get("follow_up_error", {})
        if result.returncode and not (result.returncode == 3 and follow_up.get("code") == "unavailable"):
            raise RuntimeError(f"{machine['name']}: rejoin failed: {result.stdout[-800:]} {result.stderr[-800:]}")
        deadline = time.monotonic() + 120
        while self.command("cli", "--timeout", 120, self.manifest, "--", "--json", "server", "set", machine["name"],
                           "--accepts-services=true", check=False).returncode:
            if time.monotonic() > deadline:
                raise RuntimeError(f"{machine['name']}: --accepts-services=true never took after the rejoin")
            time.sleep(2)

    def outside_scenario(self):
        machine, name = self.second, self.prefix + "-outside"
        before = self.daemon_pid(machine)
        self.cli("server", "rm", machine["name"], "--confirm", machine["name"])
        try:
            self.restarted(machine, before)
            reply = self.local_rpc(machine, "set_volume_copy", dict(
                name=name, expect=dict(kind="none"), want=dict(kind="forget"), gen=5), check=False)
            output = reply.stdout + reply.stderr
            assert reply.returncode and "not participating" in output, f"Expected the not-participating refusal: {output}"
            floors = self.floors(machine)
            assert name not in floors, f"The refused request left a floor on {machine['name']}: {floors}"
            return dict(refusal=output.strip()[-300:], floors=floors)
        finally:
            self.rejoin(machine)

    def stop_scenario(self):
        machine, volume = self.first, "data"
        self.cli("server", "set", self.second["name"], "--accepts-services=false")
        self.cli("project", "new", self.prefix)
        self.cli("link", "--project", self.prefix)
        self.cli("service", "add", "holder", "--image", "alpine:3.20")
        self.cli("set", f"""holder.startCommand=sh -c 'trap "sleep {HOLDER_STOP_SECS}; exit 0" TERM; while :; do sleep 1; done'""")
        self.cli("volume", "add", volume, "--size", "0.5GB", "--mount", "holder:/data")
        self.cli("deploy", timeout=900)
        self.cli("server", "set", self.second["name"], "--accepts-services=true")
        inspect = self.cli("volume", "inspect", volume)
        phys = f"{inspect['environment']['project']}-{inspect['environment']['name']}_vol-{inspect['id']}"
        copy = json.loads(self.local_rpc(machine, "inspect_volume_copy", dict(name=phys)).stdout)
        copy = copy["response"]["payload"]["copy"]
        assert copy["kind"] == "root", f"Expected the Writer's root copy on {machine['name']}: {copy}"
        cut_gen = self.floors(machine).get(phys, 0) + 1_000
        cut = json.dumps(dict(command="set_volume_copy", payload=dict(
            name=phys, expect=dict(kind="writer", epoch=copy["epoch"]),
            want=dict(kind="mirror", **{"from": None}, target=f"f-{cut_gen}", drain=0), gen=cut_gen)))
        floor = json.dumps(dict(command="set_volume_copy", payload=dict(
            name=phys, expect=dict(kind="writer", epoch=copy["epoch"]), want=dict(kind="floor"), gen=cut_gen + 1)))
        script = f"""
holders=$(/usr/bin/docker ps -q --no-trunc --filter volume={shlex.quote(phys)})
test -n "$holders"
ployz --json debug local-rpc {shlex.quote(cut)} >/tmp/floor-cut.out 2>&1 &
cut=$!
sleep {FLOOR_AFTER_SECS}
echo "sent $(date +%s.%N)"
floor_exit=0; ployz --json debug local-rpc {shlex.quote(floor)} >/tmp/floor-floor.out 2>&1 || floor_exit=$?
echo "replied $(date +%s.%N)"
cut_exit=0; wait "$cut" || cut_exit=$?
echo "floor_exit $floor_exit"
echo "cut_exit $cut_exit"
for holder in $holders; do
  echo "finished $(date +%s.%N -d "$(/usr/bin/docker inspect -f '{{{{.State.FinishedAt}}}}' "$holder")")"
  echo "status $(/usr/bin/docker inspect -f '{{{{.State.Status}}}}' "$holder")"
done
"""
        lines = [line.split(" ", 1) for line in self.guest(machine, script).stdout.splitlines() if " " in line]
        timing = {key: value for key, value in lines if key not in ("finished", "status")}
        timing.update(finished=[float(value) for key, value in lines if key == "finished"],
                      status=[value for key, value in lines if key == "status"],
                      sent=float(timing["sent"]), replied=float(timing["replied"]),
                      floor=self.guest(machine, "cat /tmp/floor-floor.out").stdout,
                      cut=self.guest(machine, "cat /tmp/floor-cut.out").stdout)
        finished = max(timing["finished"])
        assert timing["floor_exit"] == "0", f"The Floor failed: {timing['floor']}"
        assert set(timing["status"]) == {"exited"}, f"The cut did not stop the holder: {timing}"
        assert timing["sent"] < finished, f"The holder stopped before the Floor was sent, so nothing raced: {timing}"
        assert finished <= timing["replied"], f"The Floor replied before the holder stop finished: {timing}"
        assert timing["cut_exit"] != "0" and "stale" in timing["cut"], f"The Floor did not fence the cut: {timing['cut']}"
        return dict(floor_waited_secs=round(timing["replied"] - timing["sent"], 3),
                    stop_finished_before_reply_secs=round(timing["replied"] - finished, 3),
                    cut=timing["cut"].strip()[-300:])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--scenario", choices=("outside", "stop", "all"), default="all")
    args = parser.parse_args()
    run = json.loads(args.manifest.read_text())
    if run["provider"] not in ["incus", "boat"] or run["state"] != "ready" or len(run["machines"]) < 2 or "cloud" in run:
        raise RuntimeError("Use a ready two-Machine Incus or Boat vc without Cloud")
    checks = VolumeFloors(args.manifest)
    results = {}
    if args.scenario in ("outside", "all"):
        results["outside"] = checks.outside_scenario()
    if args.scenario in ("stop", "all"):
        results["stop"] = checks.stop_scenario()
    print(json.dumps(dict(manifest=str(args.manifest), results=results)), flush=True)


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, RuntimeError, OSError, KeyError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"volume-floors: {error}", file=sys.stderr)
        sys.exit(1)
