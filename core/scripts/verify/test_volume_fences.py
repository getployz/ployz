#!/usr/bin/env python3
"""Exercise the vc checks with a command model, without Docker, Incus or ZFS."""

from contextlib import redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


spec = importlib.util.spec_from_file_location("volume_fences", Path(__file__).resolve().parents[1] /
                                            "test-verify-volume-fences.py")
volume_fences = importlib.util.module_from_spec(spec)
spec.loader.exec_module(volume_fences)


class CommandModel:
    def __init__(self, mutation=None):
        self.mutation = mutation
        self.record = "-"
        self.volumes = set()
        self.datasets = set()
        self.slot = None
        self.joined = True

    def __call__(self, args, **kwargs):
        command = args[args.index("--") + 1:]
        if args[1] == "cli":
            code, output = self.cli(command)
        elif args[1] == "exec":
            code, output = self.guest(command)
        else:
            raise AssertionError(f"Unexpected helper command: {args}")
        return subprocess.CompletedProcess(args, code, output, "planted failure" if code else "")

    def cli(self, args):
        if args[1:3] == ["debug", "volume-rpc"]:
            request = json.loads(args[-1])
            payload = request["payload"]
            if request["command"] == "adopt_lease":
                prior = 0 if self.record == "-" else int(self.record.split(":")[0])
                if payload["lease"] < prior and self.mutation != "stale-admitted":
                    reason = "expired" if self.mutation == "wrong-refusal" else "stale_lease"
                    return 1, json.dumps(dict(error=dict(details=dict(reason=reason))))
                self.record = f"{payload['lease']}:2.0.0:closed"
                body = dict(decision="adopt")
            elif request["command"] == "inspect_volume_copy":
                assert self.slot == payload["name"]
                body = dict(copy=dict(kind="slot", readonly=True))
            else:
                raise AssertionError(request)
            return 0, json.dumps(dict(response=dict(payload=body)))
        if "server" in args:
            verb = args[args.index("server") + 1]
            if verb == "rm":
                self.joined = False
                if self.mutation == "reset-deleted-record":
                    self.record = "-"
                return 0, "{}"
            if verb == "add":
                self.joined = True
                if self.mutation == "join-deleted-record":
                    self.record = "-"
                return 0, json.dumps(dict(server=dict(machine=dict(id="rejoined-id"))))
            if verb == "ls":
                assert self.joined
                return 0, json.dumps(dict(servers=[dict(machine=dict(id="rejoined-id"), membership="up")]))
        raise AssertionError(f"Unexpected CLI command: {args}")

    def guest(self, args):
        if args[:3] == ["docker", "volume", "create"]:
            name = args[-1]
            if name.endswith("-normal") and self.mutation == "slot-poisons-create":
                return 1, ""
            self.volumes.add(name)
            self.datasets.add("ployz/ployz/" + name)
            return 0, name + "\n"
        if args[:3] == ["docker", "volume", "rm"]:
            name = args[-1]
            self.volumes.remove(name)
            if self.mutation != "remove-keeps-dataset":
                self.datasets.remove("ployz/ployz/" + name)
            if name.endswith("-lease") and self.mutation == "remove-deleted-record":
                self.record = "-"
            return 0, name + "\n"
        if args[:3] == ["docker", "volume", "ls"]:
            names = sorted(self.volumes)
            if self.slot and self.mutation == "slot-listed":
                names.append(self.slot)
            return 0, "\n".join(names) + "\n"
        if args[:2] == ["docker", "run"]:
            assert args[args.index("-v") + 1].split(":")[0] in self.volumes
            return 0, "bad write" if self.mutation == "bad-mount-write" else "volume-fence-ok"
        if args[:2] == ["zfs", "get"]:
            assert args[-1] == "ployz/ployz" and args[-2].startswith("ployz:lease.")
            return 0, self.record + "\n"
        if args[:2] == ["zfs", "create"]:
            dataset = args[-1]
            self.datasets.add(dataset)
            if dataset.endswith("/fs"):
                assert "readonly=on" in args and "refquota=67108864" in args
                self.slot = dataset.split("/")[-2]
            return 0, ""
        if args[:2] == ["zfs", "list"]:
            dataset = args[-1]
            if dataset not in self.datasets:
                return 1, ""
            if "name,readonly,refquota" in args:
                properties = "off\t67108864" if self.mutation == "malformed-slot" else "on\t67108864"
                return 0, dataset + "\t" + properties + "\n"
            return 0, dataset + "\n"
        if args[:2] == ["zfs", "destroy"]:
            dataset = args[-1]
            self.datasets = {name for name in self.datasets if name != dataset and not name.startswith(dataset + "/")}
            self.slot = None
            return 0, ""
        raise AssertionError(f"Unexpected guest command: {args}")


class VolumeFenceTests(unittest.TestCase):
    def check_scenario(self, scenario, mutation=None):
        with tempfile.TemporaryDirectory() as directory, redirect_stdout(io.StringIO()):
            manifest = Path(directory) / "cluster.json"
            manifest.write_text(json.dumps(dict(machines=[
                dict(name="machine-1", address="192.0.2.1", machine_id="original-id"),
                dict(name="machine-2", address="192.0.2.2", machine_id="survivor-id")])))
            model = CommandModel(mutation)
            checks = volume_fences.VolumeFences(manifest, prefix="model", runner=model)
            result = getattr(checks, scenario + "_scenario")()
            if scenario == "lease":
                self.assertEqual(json.loads(manifest.read_text())["machines"][0]["machine_id"], "rejoined-id")
            else:
                self.assertIsNone(model.slot)
                self.assertEqual(model.volumes, set())
            return result

    def test_lease_survives_remove_reset_and_rejects_old_request(self):
        result = self.check_scenario("lease")
        self.assertEqual(result["after_rejoin"], "2:2.0.0:closed")
        self.assertEqual(result["stale_lease"], "refused")

    def test_deleted_record_fails_at_each_lifecycle_boundary(self):
        for mutation in ("remove-deleted-record", "reset-deleted-record", "join-deleted-record"):
            with self.subTest(mutation=mutation), self.assertRaisesRegex(AssertionError, "Lease record lost"):
                self.check_scenario("lease", mutation)

    def test_old_request_must_fail_for_stale_lease(self):
        for mutation in ("stale-admitted", "wrong-refusal"):
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.check_scenario("lease", mutation)

    def test_readonly_slot_hidden_and_normal_volume_works(self):
        result = self.check_scenario("slot")
        self.assertTrue(result["hidden"])

    def test_malformed_or_listed_slot_fails(self):
        for mutation in ("malformed-slot", "slot-listed"):
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.check_scenario("slot", mutation)

    def test_slot_must_allow_a_normal_volume(self):
        with self.assertRaisesRegex(RuntimeError, "Command failed"):
            self.check_scenario("slot", "slot-poisons-create")
        with self.assertRaisesRegex(AssertionError, "did not mount"):
            self.check_scenario("slot", "bad-mount-write")

    def test_remove_must_destroy_the_dataset(self):
        for scenario in ("lease", "slot"):
            with self.subTest(scenario=scenario), self.assertRaisesRegex(AssertionError, "left.*dataset"):
                self.check_scenario(scenario, "remove-keeps-dataset")


if __name__ == "__main__":
    unittest.main()
