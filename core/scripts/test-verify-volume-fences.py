#!/usr/bin/env python3
"""Check that a readonly slot stays hidden from Docker on an existing Incus or Boat vc.

Build vc with PLOYZ_VERIFY_CARGO_ARGS='--features ployz/verify-faults,ployzd/verify-faults'.
For an Incus vc, run under the same /tmp/ployz-warm-move-vc.lock held for vc up and down; a Boat vc skips it:
  python3 scripts/test-verify-volume-fences.py <manifest> [--scenario slot|all]
"""

import argparse
import json
from pathlib import Path
import subprocess
import sys
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
        self.root = f"{pool}/ployz"
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
        assert not any(name == self.slot or name.startswith(self.slot + "/") for name in listing), listing
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
    parser.add_argument("--scenario", choices=("slot", "all"), default="all")
    parser.add_argument("--pool", default="ployz")
    args = parser.parse_args()
    run = json.loads(args.manifest.read_text())
    if run["provider"] not in ["incus", "boat"] or run["state"] != "ready" or not run["machines"]:
        raise RuntimeError("Use a ready Incus or Boat vc")
    checks = VolumeFences(args.manifest, pool=args.pool)
    results = dict(slot=checks.slot_scenario())
    print(json.dumps(dict(manifest=str(args.manifest), results=results)), flush=True)


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, RuntimeError, OSError, subprocess.TimeoutExpired) as error:
        print(f"volume-fences: {error}", file=sys.stderr)
        sys.exit(1)
