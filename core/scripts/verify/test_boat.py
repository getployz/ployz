#!/usr/bin/env python3
"""Boat provider logic that needs no sandbox: endpoints, errors, limits, placement, and which sandboxes down removes."""

import contextlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
import unittest.mock

sys.dont_write_bytecode = True
here = Path(__file__).resolve().parent
sys.path.insert(0, str(here))
import boat  # noqa: E402

loader = importlib.machinery.SourceFileLoader("verify_cluster", str(here.parent / "verify-cluster"))
cluster = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
loader.exec_module(cluster)


def result(code, *lines):
    return subprocess.CompletedProcess([], code, "\n".join(json.dumps(line) for line in lines).encode(), b"")


class Endpoints(unittest.TestCase):
    def test_wireguard_endpoint_brackets_ipv6_only(self):
        self.assertEqual(cluster.endpoint("10.1.2.3"), "10.1.2.3:51820")
        self.assertEqual(cluster.endpoint("2001:41d0:1004:389e:100::28"), "[2001:41d0:1004:389e:100::28]:51820")

    def test_ssh_endpoint_splits_host_and_port(self):
        self.assertEqual(boat.endpoint("213.32.24.158:19040"), ("213.32.24.158", 19040))


class Errors(unittest.TestCase):
    def test_the_final_error_event_wins(self):
        found = boat.error(result(1, {"event": "created", "id": "bx_1"},
                                  {"event": "error", "code": "boat_starting", "status": 409}))
        self.assertEqual((found["code"], found["status"]), ("boat_starting", 409))

    def test_success_and_plain_text_carry_no_error(self):
        self.assertIsNone(boat.error(result(0, {"event": "ready", "id": "bx_1"})))
        self.assertIsNone(boat.error(subprocess.CompletedProcess([], 1, b"not json\n", b"")))

    def test_a_failed_exec_reports_its_stderr(self):
        ran = dict(exitCode=1, stderr="Failed to start ssh.socket: Transport endpoint is not connected\n", stdout="")
        failed = subprocess.CompletedProcess([], 1, json.dumps(ran).encode(), b"")
        self.assertEqual(boat.exited(failed), "exit 1: Failed to start ssh.socket: Transport endpoint is not connected")
        self.assertIsNone(boat.exited(result(0, dict(exitCode=0, stderr=""))))


class Limits(unittest.TestCase):
    def limits(self, active=0, minute=12, hour=60, day=200):
        return dict(maxActiveSandboxes=100, activeSandboxes=active,
                    starts=dict(minute=dict(remaining=minute), hour=dict(remaining=hour), day=dict(remaining=day), unlimited=False))

    def test_a_run_that_fits_has_no_shortfall(self):
        self.assertEqual(boat.shortfalls(3, self.limits()), [])

    def test_headroom_and_start_budget_each_refuse(self):
        problems = boat.shortfalls(3, self.limits(active=98, minute=2))
        self.assertEqual(len(problems), 2)
        self.assertIn("Active sandboxes: need 3, 2 of 100 free", problems)
        self.assertIn("Starts this minute: need up to 9, 2 left", problems)

    def test_hour_and_day_budget_every_relaunch(self):
        problems = boat.shortfalls(3, self.limits(hour=8, day=9))
        self.assertEqual(problems, ["Starts this hour: need up to 9, 8 left"])

    def test_an_unlimited_account_skips_the_start_budget(self):
        limits = self.limits(hour=0)
        limits["starts"]["unlimited"] = True
        self.assertEqual(boat.shortfalls(3, limits), [])


class Images(unittest.TestCase):
    def test_boot_pulls_what_the_incus_image_bakes(self):
        script = (here / "prepare-guest.sh").read_text()
        baked = re.findall(r"docker pull (\S+)$", script, re.M)
        baked += re.search(r"for image in ([^;]+); do", script).group(1).split()
        self.assertEqual(sorted(image for image in baked if not image.startswith('"')), sorted(boat.IMAGES))


class Registry(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.saved = boat.REGISTRY
        boat.REGISTRY = Path(directory.name)
        self.addCleanup(setattr, boat, "REGISTRY", self.saved)
        self.calls = []
        self.answers = {}
        self.run = dict(id="run1", manifest=str(Path(directory.name) / "cluster.json"), machines=[])
        self.provider = boat.Provider(self.run, self.fake, Path("."))

    def fake(self, args, check=True, **kwargs):
        self.calls.append(args[2:-1])
        return self.answers.get(tuple(args[2:4]), result(0, {}))

    def registered(self, **sandboxes):
        self.provider.create()
        for name, entry in sandboxes.items():
            self.provider.mark(name, **entry)

    def test_down_deletes_every_registered_sandbox_and_forgets_the_run(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running"), "machine-2": dict(id="bx_b", state="running")})
        self.provider.destroy()
        self.assertEqual(self.calls, [["delete", "bx_a", "--yes"], ["delete", "bx_b", "--yes"]])
        self.assertFalse(boat.registry("run1").exists())

    def test_a_forbidden_delete_stops_the_sandbox_and_keeps_it_registered(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running")})
        self.answers[("delete", "bx_a")] = result(1, {"event": "error", "code": "api_key_action_forbidden", "status": 403})
        with contextlib.redirect_stderr(io.StringIO()) as said:
            self.provider.destroy()
        self.assertEqual(self.calls, [["delete", "bx_a", "--yes"], ["stop", "bx_a"]])
        self.assertIn("sandbox.delete-own", said.getvalue())
        self.assertEqual(json.loads(boat.registry("run1").read_text())["sandboxes"]["machine-1"]["state"], "stopped")

    def test_a_failed_delete_still_deletes_the_rest_then_fails(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running"), "machine-2": dict(id="bx_b", state="running")})
        self.answers[("delete", "bx_a")] = result(1, {"event": "error", "code": "internal", "status": 500})
        with self.assertRaisesRegex(RuntimeError, "bx_a"):
            self.provider.destroy()
        self.assertEqual(self.calls, [["delete", "bx_a", "--yes"], ["delete", "bx_b", "--yes"]])
        self.assertEqual(json.loads(boat.registry("run1").read_text())["sandboxes"]["machine-2"]["state"], "deleted")

    def test_a_rate_limited_create_waits_and_retries(self):
        self.registered()
        self.run["image"] = "ployz-verify-024"
        answers = iter([result(1, {"event": "error", "code": "rate_limited", "status": 429}),
                        result(0, {"event": "created", "id": "bx_new"})])
        self.provider.commands = lambda args, check=True, **kwargs: next(answers)
        with unittest.mock.patch("boat.time.sleep") as slept:
            self.assertEqual(self.provider.launch(dict(name="machine-1")), "bx_new")
        slept.assert_called_once()

    def test_down_touches_no_sandbox_outside_the_registry(self):
        boat.write(boat.registry("other"), dict(run="other", sandboxes={"machine-1": dict(id="bx_z", state="running")}))
        self.registered(**{"machine-1": dict(id="bx_a", state="deleted")})
        self.provider.destroy()
        self.assertEqual(self.calls, [])
        self.assertTrue(boat.registry("other").exists())

    def test_a_sandbox_on_a_peers_node_is_refused(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running", ip="2001:41d0:1004:4161:100::33"),
                           "machine-2": dict(id="bx_b", state="running")})
        crowded = dict(ip="2001:41d0:1004:4161:100::23", sshEndpoint="213.32.27.97:19035")
        self.assertFalse(self.provider.place("machine-2", crowded))
        self.assertNotIn("ip", self.provider.record()["sandboxes"]["machine-2"])

    def test_a_sandbox_on_its_own_node_is_recorded(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running", ip="2001:41d0:1004:4161:100::33"),
                           "machine-2": dict(id="bx_b", state="running"),
                           "machine-3": dict(id="bx_c", state="deleted", ip="2001:41d0:1004:389e:100::28")})
        apart = dict(ip="2001:41d0:1004:389e:100::40", sshEndpoint="213.32.24.158:19040")
        self.assertTrue(self.provider.place("machine-2", apart))
        self.assertEqual(self.provider.record()["sandboxes"]["machine-2"]["ssh"], "213.32.24.158:19040")


if __name__ == "__main__":
    unittest.main()
