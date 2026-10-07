#!/usr/bin/env python3
"""Check lease persistence and hidden slots on an existing two-Machine Incus vc.

Build vc with PLOYZ_VERIFY_CARGO_ARGS='--features ployz/verify-faults,ployzd/verify-faults'.
Run under the same /tmp/ployz-warm-move-vc.lock held for vc up and down:
  python3 scripts/test-verify-volume-fences.py <manifest> [--scenario lease|slot|all]
The lease scenario removes and rejoins machine-1. It needs a standalone vc.
"""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True

CORE = Path(__file__).resolve().parents[1]
HELPER = CORE / "scripts" / "verify-cluster"


class VolumeFences:
    def __init__(self, manifest, *, pool="ployz", prefix=None, runner=subprocess.run):
        self.manifest = Path(manifest)
        self.run = json.loads(self.manifest.read_text())
        self.pool = pool
        self.prefix = prefix or "vc-fence-" + uuid.uuid4().hex[:12]
        self.runner = runner
        self.machine = self.run["machines"][0]
        self.survivor = self.run["machines"][1]
        self.root = f"{pool}/ployz"
        self.name = self.prefix + "-lease"
        self.slot = self.prefix + "-slot"
        self.normal = self.prefix + "-normal"

    def command(self, *args, check=True, timeout=600):
        command = [str(HELPER), *map(str, args)]
        result = self.runner(command, capture_output=True, text=True, timeout=timeout + 30)
        print(json.dumps(dict(command=command, exit_code=result.returncode,
                              stdout=result.stdout, stderr=result.stderr)), flush=True)
        if check and result.returncode:
            raise RuntimeError(f"Command failed: {command}\n{result.stdout}\n{result.stderr}")
        return result

    def guest(self, *args, check=True):
        return self.command("exec", self.manifest, self.machine["name"], "--", *args, check=check)

    def cli(self, *args, check=True):
        return self.command("cli", "--timeout", "600", self.manifest, "--", *args, check=check)

    def rpc(self, command, payload, *, check=True):
        request = json.dumps(dict(command=command, payload=payload))
        result = self.cli("--json", "debug", "volume-rpc", self.machine["name"], request, check=check)
        return result.returncode, json.loads(result.stdout)

    def adopt(self, lease, *, check=True):
        return self.rpc("adopt_lease", dict(name=self.name, lease=lease,
                                           not_after_unix_seconds=int(time.time()) + 3600), check=check)

    def record(self, minimum=2):
        value = self.guest("zfs", "get", "-H", "-o", "value", "ployz:lease." + self.name,
                           self.root).stdout.strip()
        match = re.fullmatch(r"([0-9]+):([0-9]+)\.([0-9]+)\.([0-9]+):(open|closed)", value)
        assert match and int(match[1]) >= minimum, f"Lease record lost or regressed: {value!r}"
        return value

    def prepare_lease(self):
        self.guest("docker", "volume", "create", "-d", "ployz", "-o", "size=64m", self.name)
        for lease in (1, 2):
            _, reply = self.adopt(lease)
            assert reply["response"]["payload"]["decision"] == "adopt", reply
        record = self.record()
        assert record == "2:2.0.0:closed", record
        return record

    def remove_volume(self, before):
        self.guest("docker", "volume", "rm", self.name)
        assert self.record() == before, "Docker Remove changed the lease record"
        result = self.guest("zfs", "list", "-H", "-o", "name", f"{self.root}/{self.name}", check=False)
        assert result.returncode != 0, "Docker Remove left the Volume dataset"

    def remove_machine(self):
        before = self.record()
        self.cli("--json", "--connect", "ssh://root@" + self.survivor["address"],
                 "server", "rm", self.machine["name"], "--confirm", self.machine["name"])
        return self.record(int(before.split(":", 1)[0]))

    def rejoin_machine(self, before):
        deadline = time.monotonic() + 120
        while True:
            result = self.cli("--json", "server", "add", "root@" + self.machine["address"],
                              "--standalone", "--no-install", "--storage", "none", "--yes",
                              "--name", self.machine["name"], "--ssh-key",
                              str(self.manifest.parent / "private" / "id_ed25519"), "--public-ip", "none",
                              "--wg-endpoint", self.machine["address"] + ":51820", "--wg-mtu", "1420", check=False)
            reply = json.loads(result.stdout)
            if result.returncode == 0:
                break
            if reply.get("error", {}).get("code") != "unavailable" or time.monotonic() >= deadline:
                raise RuntimeError(f"Machine rejoin failed: {reply}")
            time.sleep(1)
        new_id = reply["server"]["machine"]["id"]
        self.machine["machine_id"] = new_id
        temporary = self.manifest.with_suffix(".tmp")
        temporary.write_text(json.dumps(self.run, indent=2) + "\n")
        temporary.replace(self.manifest)
        deadline = time.monotonic() + 60
        while True:
            result = self.cli("--json", "server", "ls", check=False)
            if result.returncode == 0 and any(server["machine"]["id"] == new_id and server["membership"] == "up"
                                              for server in json.loads(result.stdout)["servers"]):
                break
            assert time.monotonic() < deadline, "Rejoined Machine never became ready"
            time.sleep(1)
        return self.record(int(before.split(":", 1)[0]))

    def stale_lease(self):
        before = self.record()
        code, reply = self.adopt(1, check=False)
        assert code != 0 and reply.get("error", {}).get("details", {}).get("reason") == "stale_lease", reply
        assert self.record() == before, "The stale request changed the lease record"

    def lease_scenario(self):
        before = self.prepare_lease()
        self.remove_volume(before)
        removed = self.remove_machine()
        rejoined = self.rejoin_machine(removed)
        self.stale_lease()
        return dict(before=before, after_remove=removed, after_rejoin=rejoined, stale_lease="refused")

    def prepare_slot(self):
        seed = self.prefix + "-seed"
        self.guest("docker", "volume", "create", "-d", "ployz", "-o", "size=64m", seed)
        self.guest("docker", "volume", "rm", seed)
        parent = f"{self.pool}/ployz-mirror"
        if self.guest("zfs", "list", "-H", "-o", "name", parent, check=False).returncode:
            self.guest("zfs", "create", "-o", "canmount=off", "-o", "readonly=on", "-o",
                       "mountpoint=/var/lib/ployz-mirror", parent)
        self.guest("zfs", "create", "-o", "canmount=off", f"{parent}/{self.slot}")
        self.guest("zfs", "create", "-o", "canmount=off", "-o", "readonly=on", "-o",
                   "refquota=67108864", f"{parent}/{self.slot}/fs")

    def hidden_slot(self):
        fs = f"{self.pool}/ployz-mirror/{self.slot}/fs"
        properties = self.guest("zfs", "list", "-Hp", "-o", "name,readonly,refquota", fs).stdout.strip()
        assert properties == f"{fs}\ton\t67108864", f"Malformed slot: {properties!r}"
        _, reply = self.rpc("inspect_volume_copy", dict(name=self.slot))
        copy = reply["response"]["payload"]["copy"]
        assert copy and copy["kind"] == "slot" and copy["readonly"] is True, reply
        names = self.guest("docker", "volume", "ls", "-q").stdout.splitlines()
        assert not any(name == self.slot or name.startswith(self.slot + "/") for name in names), names

    def normal_volume(self):
        self.guest("docker", "volume", "create", "-d", "ployz", "-o", "size=64m", self.normal)
        listing = self.guest("docker", "volume", "ls", "-q").stdout.splitlines()
        assert self.normal in listing, "Docker did not list the ordinary Volume"
        result = self.guest("docker", "run", "--rm", "-v", self.normal + ":/data", "alpine:3.20",
                            "sh", "-ec", "printf volume-fence-ok > /data/marker; sync; cat /data/marker")
        assert result.stdout.strip() == "volume-fence-ok", "The ordinary Volume did not mount and retain its write"
        self.guest("docker", "volume", "rm", self.normal)
        assert self.normal not in self.guest("docker", "volume", "ls", "-q").stdout.splitlines()
        result = self.guest("zfs", "list", "-H", "-o", "name", f"{self.root}/{self.normal}", check=False)
        assert result.returncode != 0, "Docker Remove left the ordinary Volume dataset"
        self.hidden_slot()

    def slot_scenario(self):
        self.prepare_slot()
        try:
            self.hidden_slot()
            self.normal_volume()
            return dict(slot=self.slot, hidden=True, normal_volume="created, mounted, written, removed")
        finally:
            self.guest("zfs", "destroy", "-r", f"{self.pool}/ployz-mirror/{self.slot}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--scenario", choices=("lease", "slot", "all"), default="all")
    parser.add_argument("--pool", default="ployz")
    args = parser.parse_args()
    run = json.loads(args.manifest.read_text())
    if run["provider"] != "incus" or run["state"] != "ready" or len(run["machines"]) < 2:
        raise RuntimeError("Use a ready Incus vc with at least two Machines")
    if args.scenario in ("lease", "all") and "cloud" in run:
        raise RuntimeError("The lease reset scenario needs a standalone vc")
    checks = VolumeFences(args.manifest, pool=args.pool)
    results = {}
    if args.scenario in ("lease", "all"):
        results["lease"] = checks.lease_scenario()
    if args.scenario in ("slot", "all"):
        results["slot"] = checks.slot_scenario()
    print(json.dumps(dict(manifest=str(args.manifest), results=results)), flush=True)


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, RuntimeError, OSError, subprocess.TimeoutExpired) as error:
        print(f"volume-fences: {error}", file=sys.stderr)
        sys.exit(1)
