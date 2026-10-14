#!/usr/bin/env python3
"""Boat provider logic that needs no sandbox: endpoints, errors, limits, placement, and which sandboxes down removes."""

import contextlib
import fcntl
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
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
        self.assertIn("Starts this minute: need up to 3, 2 left", problems)

    def test_the_minute_budgets_one_start_per_machine(self):
        self.assertEqual(boat.shortfalls(8, self.limits(minute=12, hour=60, day=200)), [])

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


class Bakes(unittest.TestCase):
    def bake(self, day):
        return f"{boat.BAKED}{day}" + "0" * 8

    def test_prune_keeps_the_newest_bakes_and_never_names_another_snapshot(self):
        bakes = [self.bake(day) for day in ["11", "13", "10", "12"]]
        named = [dict(name="ployz-verify-024", createdAt="2026-01-01"), dict(name="someone-else", createdAt="2026-01-02"),
                 *(dict(name=name, createdAt=f"2026-10-{name[-10:-8]}") for name in bakes)]
        self.assertEqual(boat.stale_bakes(named, self.bake("14"), bakes, set()), [self.bake("10"), self.bake("11")])

    def test_a_full_account_prunes_older_bakes_to_make_room(self):
        others = [dict(name=f"other-{n}", createdAt="2026-01-01") for n in range(8)]
        bakes = [self.bake(day) for day in ["10", "11"]]
        named = [*others, *(dict(name=name, createdAt=f"2026-10-{name[-10:-8]}") for name in bakes)]
        self.assertEqual(boat.stale_bakes(named, self.bake("14"), bakes, set()), [self.bake("10")])

    def test_an_unfinished_current_bake_takes_no_extra_slot(self):
        others = [dict(name=f"other-{n}", createdAt="2026-01-01") for n in range(7)]
        bakes = [self.bake(day) for day in ["10", "11", "14"]]
        named = [*others, *(dict(name=name, createdAt=f"2026-10-{name[-10:-8]}") for name in bakes)]
        self.assertEqual(boat.stale_bakes(named, self.bake("14"), bakes, set()), [])

    def test_the_current_bake_is_never_pruned(self):
        bakes = [self.bake(day) for day in ["10", "11", "12", "13"]]
        named = [dict(name=name, createdAt=f"2026-10-{name[-10:-8]}") for name in bakes]
        self.assertNotIn(self.bake("10"), boat.stale_bakes(named, self.bake("10"), bakes, set()))

    def test_a_bake_another_host_made_is_never_pruned(self):
        bakes = [self.bake(day) for day in ["10", "11", "12", "13"]]
        named = [dict(name=name, createdAt=f"2026-10-{name[-10:-8]}") for name in bakes]
        self.assertEqual(boat.stale_bakes(named, self.bake("14"), bakes[1:], set()), [self.bake("11")])

    def test_a_bake_a_live_run_uses_is_never_pruned(self):
        bakes = [self.bake(day) for day in ["10", "11", "12", "13"]]
        named = [dict(name=name, createdAt=f"2026-10-{name[-10:-8]}") for name in bakes]
        self.assertEqual(boat.stale_bakes(named, self.bake("14"), bakes, {self.bake("10")}), [self.bake("11")])

    def test_a_name_that_only_shares_the_prefix_is_never_pruned(self):
        named = [dict(name=f"{boat.BAKED}mine-keep", createdAt="2026-10-01"),
                 *(dict(name=self.bake(day), createdAt=f"2026-10-{day}") for day in ["10", "11", "12"])]
        ours = [f"{boat.BAKED}mine-keep", *(self.bake(day) for day in ["10", "11", "12"])]
        self.assertEqual(boat.stale_bakes(named, self.bake("14"), ours, set()), [self.bake("10")])


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
        answer = self.answers.get(tuple(args[2:4]), result(0, {}))
        if check and answer.returncode:
            raise RuntimeError(f"{args[2:-1]} failed")
        return answer

    def registered(self, **sandboxes):
        self.provider.create()
        for name, entry in sandboxes.items():
            self.provider.mark(name, **entry)

    def test_registry_changes_and_its_removal_hold_the_host_wide_registry_lock(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running")})
        held, unlink = [], Path.unlink
        self.provider.record(lambda record: held.append(locked("registry.lock")))

        def unlinked(path, *args, **kwargs):
            held.append(locked("registry.lock"))
            unlink(path, *args, **kwargs)
        with unittest.mock.patch.object(Path, "unlink", unlinked):
            self.provider.destroy()
        self.assertEqual(held, [True, True])

    def test_refresh_after_down_removed_the_registry_neither_fails_nor_recreates_it(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running")})
        self.provider.record(lambda record: record.update(extended=0))
        boat.registry("run1").unlink()
        self.provider.refresh()
        self.assertFalse(boat.registry("run1").exists())
        self.assertEqual(self.calls, [])

    def test_recording_a_sandbox_after_down_removed_the_registry_fails_instead_of_losing_it(self):
        self.registered()
        boat.registry("run1").unlink()
        with self.assertRaisesRegex(RuntimeError, "registry for this run is gone"):
            self.provider.mark("machine-1", id="bx_a", state="running")
        self.assertFalse(boat.registry("run1").exists())

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

    def test_the_ssh_control_socket_fits_a_unix_socket_path(self):
        runtime = tempfile.mkdtemp(dir="/tmp")
        self.addCleanup(shutil.rmtree, runtime)
        # The accented directory's socket directory is 40 characters, too long only in bytes; the other is too long either way.
        accented, long = Path(runtime, "é" * (39 - len(runtime) - 11)), Path(runtime, "r" * 40)
        accented.mkdir(), long.mkdir()

        def path(run, runtime=runtime):
            with unittest.mock.patch.dict("os.environ", XDG_RUNTIME_DIR=runtime):
                return boat.Provider(dict(id=run, manifest=self.run["manifest"]), self.fake, Path(".")).control_path()
        for run in ["bake-" + "f" * 10, "20261014T034406-9a4821d75f"]:
            self.assertTrue(path(run).startswith(f"{runtime}/ployz-boat/"))
            # %C expands to 40 hex characters and ssh adds a 17-character suffix while it binds.
            self.assertLessEqual(len(path(run).replace("%C", "0" * 40)) + 17, 107)
        self.assertNotEqual(path("a"), path("b"))
        self.assertTrue(path("a", str(accented)).startswith("/tmp/ployz-boat-"))
        # su without "-" keeps another user's runtime directory, and a logout removes one.
        self.assertTrue(path("a", "/run/user/4294967295").startswith(f"/tmp/ployz-boat-{os.getuid()}/"))
        Path(runtime, "file").touch()
        self.assertTrue(path("a", f"{runtime}/file").startswith(f"/tmp/ployz-boat-{os.getuid()}/"))
        if os.getuid():
            self.assertTrue(path("a", "/").startswith(f"/tmp/ployz-boat-{os.getuid()}/"))
        long = path("a", str(long))
        self.assertTrue(long.startswith(f"/tmp/ployz-boat-{os.getuid()}/"))
        self.assertLessEqual(len(long.replace("%C", "0" * 40)) + 17, 107)

    def test_a_control_directory_others_can_open_is_refused(self):
        self.provider.control = boat.REGISTRY / "control"
        self.provider.control.mkdir(mode=0o777)
        self.provider.control.chmod(0o777)
        with self.assertRaisesRegex(RuntimeError, "only you can open"):
            self.provider.options()
        self.provider.control.chmod(0o700)
        self.assertIn("ControlMaster=auto", self.provider.options())

    def test_runs_lists_each_registered_run_with_its_sandbox_state(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running")})
        listed = dict(sandboxes=[dict(id="bx_a", state="running")])
        with unittest.mock.patch.object(boat, "boat_json", return_value=listed):
            found = boat.runs()
        self.assertEqual([(run["id"], [machine["sandbox"] for machine in run["machines"]]) for run in found], [("run1", ["bx_a"])])

    def test_a_corrupt_run_record_hides_only_itself(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running")})
        (boat.REGISTRY / "broken.json").write_text("{")
        with unittest.mock.patch("sys.stderr", new_callable=io.StringIO):
            self.assertEqual([record["run"] for record in boat.records()], ["run1"])

    def test_a_failed_stop_still_cleans_the_rest_then_fails(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running"), "machine-2": dict(id="bx_b", state="running")})
        self.answers[("delete", "bx_a")] = result(1, {"event": "error", "code": "api_key_action_forbidden", "status": 403})
        self.answers[("stop", "bx_a")] = result(1, {"event": "error", "code": "internal", "status": 500})
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaisesRegex(RuntimeError, "bx_a"):
            self.provider.destroy()
        self.assertEqual(self.calls, [["delete", "bx_a", "--yes"], ["stop", "bx_a"], ["delete", "bx_b", "--yes"]])

    def test_a_machine_without_an_ssh_endpoint_raises_a_handled_error(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running")})
        with self.assertRaisesRegex(RuntimeError, "machine-1 has no SSH endpoint"):
            self.provider.exec(dict(name="machine-1"), ["true"])
        boat.registry("run1").unlink()
        with self.assertRaisesRegex(RuntimeError, "machine-1 has no SSH endpoint"):
            self.provider.exec(dict(name="machine-1"), ["true"])

    def test_teardown_destroys_even_when_evidence_fails(self):
        backend = unittest.mock.Mock()
        backend.exec.side_effect = KeyError("ssh")
        run = dict(machines=[dict(name="machine-1")], manifest=self.run["manifest"])
        with unittest.mock.patch.object(cluster, "save"), contextlib.suppress(KeyError):
            cluster.teardown(run, None, backend)
        backend.destroy.assert_called_once()

    def test_one_provider_failing_to_parse_keeps_the_other_providers_runs(self):
        for failure in [json.JSONDecodeError("notice", "", 0), KeyError("sandboxes")]:
            def provider(name):
                if name == "boat":
                    raise failure
                return unittest.mock.Mock(runs=lambda: [dict(id="incus-run")])
            with unittest.mock.patch.object(cluster.importlib, "import_module", side_effect=provider), \
                 unittest.mock.patch.object(cluster.shutil, "which", return_value="/usr/bin/incus"), \
                 contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(cluster.host_runs(), [dict(id="incus-run", provider="incus")])

    def test_prepare_spares_a_bake_a_live_run_uses_and_forgets_the_pruned_ones_fingerprint(self):
        bakes = [f"{boat.BAKED}{day}" + "0" * 8 for day in ["10", "11", "12", "13"]]
        ledger = boat.REGISTRY / "bakes.ledger"
        boat.write(ledger, [*bakes, bakes[2]])
        boat.write(boat.registry("live"), dict(run="live", image=bakes[0], sandboxes={}))
        service = boat.REGISTRY / "core/crates/ployzd/src/corrosion/service.rs"
        service.parent.mkdir(parents=True)
        service.write_text('pub const IMAGE: &str = "corrosion:test";')
        self.provider.core = boat.REGISTRY / "core"
        named = [dict(name=name, status="ready", createdAt=f"2026-10-{name[-10:-8]}") for name in bakes]
        removed = []

        def fake(*args, check=True, **kwargs):
            if args[:2] == ("snapshot", "rm"):
                removed.append(args[2])
                return result(0, {})
            raise RuntimeError("stop before baking")
        self.provider.boat = fake
        for bake in bakes[:2]:
            boat.write(boat.fingerprints(bake), {"containers": []})
        with unittest.mock.patch.object(boat, "BAKES", ledger), \
             unittest.mock.patch.object(boat, "boat_json", return_value=dict(named=named)), \
             unittest.mock.patch.object(boat, "bootstrap", return_value="key"), \
             unittest.mock.patch("boat.shutil.which", return_value="/bin/true"), \
             unittest.mock.patch.object(boat.Provider, "create", side_effect=RuntimeError("stop before baking")), \
             unittest.mock.patch.object(boat.Provider, "destroy"), \
             contextlib.suppress(RuntimeError), contextlib.redirect_stderr(io.StringIO()):
            self.provider.prepare()
        self.assertEqual(removed, [bakes[1]])
        self.assertEqual(boat.read(ledger), [bakes[0], bakes[2], bakes[3], self.run["image"]])
        self.assertEqual([boat.fingerprints(bake).exists() for bake in bakes[:2]], [True, False])

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

    def test_refresh_extends_live_sandboxes_and_names_failures_once(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running"), "machine-2": dict(id="bx_b", state="running"),
                           "machine-3": dict(id="bx_c", state="deleted")})
        self.answers[("extend", "bx_b")] = result(1, {"event": "error", "code": "internal", "status": 500})
        self.provider.record(lambda record: record.update(extended=0))
        with contextlib.redirect_stderr(io.StringIO()) as said:
            self.provider.refresh()
        self.assertEqual(sorted(self.calls), [["extend", "bx_a", "--ttl", boat.TTL], ["extend", "bx_b", "--ttl", boat.TTL]])
        self.assertEqual(said.getvalue().count("\n"), 1)
        self.assertIn("bx_b", said.getvalue())
        self.assertNotIn("bx_a", said.getvalue())

    def test_refresh_extends_only_once_half_the_ttl_is_spent_and_remembers_a_full_success(self):
        self.registered(**{"machine-1": dict(id="bx_a", state="running")})
        self.provider.refresh()
        self.assertEqual(self.calls, [])
        self.provider.record(lambda record: record.update(extended=int(time.time()) - boat.TTL // 2 - 1))
        self.provider.refresh()
        self.provider.refresh()
        self.assertEqual(self.calls, [["extend", "bx_a", "--ttl", boat.TTL]])
        self.provider.record(lambda record: record.update(extended=0))
        self.answers[("extend", "bx_a")] = result(1, {"event": "error", "code": "internal", "status": 500})
        with contextlib.redirect_stderr(io.StringIO()):
            self.provider.refresh()
            self.provider.refresh()
        self.assertEqual(len(self.calls), 3)

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


def locked(name):
    """Whether some open file holds REGISTRY/name's lock now."""
    with (boat.REGISTRY / name).open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        fcntl.flock(lock, fcntl.LOCK_UN)
        return False


def parked(sandbox, node, image="bake-a", at=0):
    return dict(id=sandbox, image=image, ip=f"2001:db8:0:{node}::1", ssh=f"203.0.113.{node}:22", parked_at=at)


class Pool(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        for name in ["REGISTRY", "POOL", "RETIRING", "BAKES"]:
            self.addCleanup(setattr, boat, name, getattr(boat, name))
            setattr(boat, name, Path(directory.name) / getattr(boat, name).name if name != "REGISTRY" else Path(directory.name))

    def provider(self, *machines, answers=None):
        self.calls = []

        def fake(args, check=True, **kwargs):
            self.calls.append(args[2:-1])
            return (answers or {}).get(tuple(args[2:4]), result(0, {}))
        run = dict(id="run1", manifest=str(boat.REGISTRY / "cluster.json"), image="bake-a",
                   machines=[dict(name=name) for name in machines])
        return boat.Provider(run, fake, Path("."))

    def locked(self):
        return locked("pool.lock")

    def test_a_claim_reads_who_owns_a_sandbox_under_the_pool_lock(self):
        held = []
        with unittest.mock.patch.object(boat, "registered", lambda: held.append(self.locked()) or set()), \
                unittest.mock.patch.object(boat, "boat_json", lambda *args: dict(sandboxes=[])):
            self.provider().create()
        self.assertEqual(held, [True])

    def test_fingerprint_ignores_hold_names_and_order_but_not_a_leftover(self):
        fresh = boat.fingerprint("== containers\nhold-1|alpine:3.20|running|1\nzed|redis|running|\n== volumes\n== zpools\n")
        reset = boat.fingerprint("== containers\nzed|redis|running|\nhold-9|alpine:3.20|running|1\n== volumes\n\n== zpools\n")
        self.assertEqual(boat.differences(fresh, reset), [])
        dirty = boat.fingerprint("== containers\nhold-9|alpine:3.20|running|1\nzed|redis|running|\nweb|nginx|exited|\n"
                                 "== volumes\nlocal data\n== zpools\n")
        self.assertEqual(boat.differences(fresh, dirty), ["containers (-[] +['web|nginx|exited|'])", "volumes (-[] +['local data'])"])
        self.assertIn("ssh (-[] +['exit 255'])", boat.differences(fresh, {**fresh, "ssh": ["exit 255"]}))

    def test_listeners_compare_across_clones_but_not_a_new_port(self):
        fresh = boat.fingerprint('== listening\nudp 0.0.0.0:57550 \nudp [::]:33831 users:(("avahi-daemon",pid=834,fd=15))\n'
                                 'tcp 0.0.0.0:22 users:(("sshd",pid=19,fd=3),("systemd",pid=1,fd=313))\n'
                                 'udp 192.168.122.49%enp7s0:68 users:(("systemd-network",pid=728,fd=23))\n')
        clone = ('== listening\ntcp 0.0.0.0:22 users:(("systemd",pid=1,fd=40),("sshd",pid=7,fd=3))\n'
                 'udp [::]:45818 users:(("avahi-daemon",pid=901,fd=15))\nudp 0.0.0.0:40543 \n'
                 'udp 192.168.122.47%enp7s0:68 users:(("systemd-network",pid=700,fd=23))\n')
        self.assertEqual(boat.differences(fresh, boat.fingerprint(clone)), [])
        leftover = boat.fingerprint(clone + 'udp 0.0.0.0:51820 \ntcp [::]:7574 users:(("ployzd",pid=5,fd=13))\n')
        self.assertEqual(boat.differences(fresh, leftover),
                         ["listening (-[] +['tcp [::]:7574 ployzd', 'udp 0.0.0.0:ephemeral'])"])
        renamed = boat.fingerprint(clone.replace('("systemd",pid=1,fd=40),', ''))
        self.assertEqual(boat.differences(fresh, renamed), ["listening (-['tcp 0.0.0.0:22 sshd systemd'] +['tcp 0.0.0.0:22 sshd'])"])

    def test_a_run_claims_its_own_bake_newest_first_one_per_node_up_to_its_size(self):
        entries = [parked("bx_old", 1, at=1), parked("bx_new", 1, at=3), parked("bx_two", 2, at=2), parked("bx_three", 3, at=0),
                   parked("bx_other", 4, image="bake-b", at=9)]
        self.assertEqual([entry["id"] for entry in boat.pick(entries, "bake-a", 2)], ["bx_new", "bx_two"])
        self.assertEqual([entry["id"] for entry in boat.pick(entries, "bake-a", 5)], ["bx_new", "bx_two", "bx_three"])

    def listing(self, **states):
        return lambda *args: dict(sandboxes=[dict(id=sandbox, state=state) for sandbox, state in states.items()])

    def test_a_claim_deletes_stale_entries_of_any_bake_and_keeps_a_sibling_bakes_live_ones(self):
        boat.write(boat.BAKES, ["bake-a", "bake-b"])
        boat.write(boat.POOL, [parked("bx_gone", 1), parked("bx_stopped", 2), parked("bx_old", 3, image="bake-c"),
                               parked("bx_sibling", 4, image="bake-b"), parked("bx_held", 5)])
        boat.write(boat.registry("run9"), dict(run="run9", image="bake-a", sandboxes={"m": dict(id="bx_held", state="running")}))
        listing = self.listing(bx_stopped="stopped", bx_old="ready", bx_sibling="ready", bx_held="ready")
        with unittest.mock.patch.object(boat, "boat_json", listing):
            provider = self.provider()
            provider.create()
        self.assertEqual([entry["id"] for entry in boat.read(boat.POOL)], ["bx_sibling"])
        self.assertEqual(sorted(self.calls), [["delete", sandbox, "--yes"] for sandbox in ["bx_gone", "bx_old", "bx_stopped"]])

    def test_a_claim_budgets_the_starts_its_discarded_candidates_need(self):
        boat.write(boat.BAKES, ["bake-a"])
        boat.write(boat.POOL, [parked("bx_a", 1)])
        limits = dict(maxActiveSandboxes=100, activeSandboxes=1, starts=dict(
            minute=dict(remaining=2), hour=dict(remaining=60), day=dict(remaining=200), unlimited=False))
        answers = dict(list=dict(sandboxes=[dict(id="bx_a", state="ready")]), limits=limits)
        with unittest.mock.patch.object(boat, "boat_json", lambda *args: answers[args[0]]), \
                contextlib.redirect_stderr(io.StringIO()) as said:
            # No fingerprint of bake-a exists, so the candidate fails its proof.
            with self.assertRaisesRegex(RuntimeError, "Starts this minute: need up to 3, 2 left"):
                self.provider("machine-1", "machine-2", "machine-3").create()
        self.assertIn("no fingerprint of this bake", said.getvalue())

    def test_a_new_fingerprint_script_or_reading_never_meets_an_old_fingerprint(self):
        saved = boat.fingerprints("bake-a")
        self.assertNotEqual(boat.fingerprints("bake-b"), saved)
        with unittest.mock.patch.object(boat, "FINGERPRINT", boat.FINGERPRINT + "section extra; true\n"):
            self.assertNotEqual(boat.fingerprints("bake-a"), saved)
        with unittest.mock.patch.object(boat, "NORMALIZE", boat.NORMALIZE + 1):
            self.assertNotEqual(boat.fingerprints("bake-a"), saved)
        self.assertEqual(boat.fingerprints("bake-a"), saved)

    def park(self, ledger, pool):
        """Park machine-1 as bx_new with POOL_SIZE 2; returns the provider."""
        boat.write(boat.BAKES, ledger)
        limits = dict(maxActiveSandboxes=100, activeSandboxes=0, starts=dict(unlimited=True))
        listing = self.listing(**{entry["id"]: "ready" for entry in pool if entry["id"] != "bx_gone"}, bx_new="ready")
        with unittest.mock.patch.object(boat, "boat_json", lambda *args: limits if args[0] == "limits" else listing()), \
                unittest.mock.patch.object(boat, "POOL_SIZE", 2), contextlib.redirect_stderr(io.StringIO()):
            provider = self.provider("machine-1")
            provider.create()
            boat.write(boat.POOL, pool)
            provider.mark("machine-1", id="bx_new", state="running", ip="2001:db8:0:9::1", ssh="203.0.113.9:22")
            boat.write(boat.fingerprints("bake-a"), {"containers": []})
            provider.reboot = lambda node: None
            provider.fingerprint = lambda node: {"containers": []}
            provider.park("machine-1")
        return provider

    def test_park_displaces_an_older_bakes_oldest_sandbox_to_keep_the_whole_pool_at_its_size(self):
        provider = self.park(["bake-b", "bake-a"], [parked("bx_b1", 1, image="bake-b", at=1),
                                                     parked("bx_b2", 2, image="bake-b", at=2), parked("bx_gone", 3)])
        self.assertEqual([entry["id"] for entry in boat.read(boat.POOL)], ["bx_b2", "bx_new"])
        self.assertEqual(sorted(call for call in self.calls if call[0] == "delete"),
                         [["delete", "bx_b1", "--yes"], ["delete", "bx_gone", "--yes"]])
        self.assertEqual(provider.record()["sandboxes"]["machine-1"]["state"], "parked")

    def test_park_never_displaces_a_newer_bake_and_keeps_each_bake_to_the_pool_size(self):
        for ledger, image in [(["bake-a", "bake-b"], "bake-b"), (["bake-b", "bake-a"], "bake-a")]:
            with self.subTest(full_of=image):
                pool = [parked("bx_1", 1, image=image), parked("bx_2", 2, image=image)]
                provider = self.park(ledger, pool)
                self.assertEqual(boat.read(boat.POOL), pool)
                self.assertEqual(provider.record()["sandboxes"]["machine-1"]["state"], "running")
                boat.registry("run1").unlink()

    def test_a_stale_entry_stays_in_the_pool_until_its_delete_succeeds_or_finds_it_gone(self):
        boat.write(boat.BAKES, ["bake-a"])
        boat.write(boat.POOL, [parked("bx_failed", 1), parked("bx_missing", 2)])
        answers = {("delete", "bx_failed"): result(1, {"event": "error", "code": "internal", "status": 500}),
                   ("delete", "bx_missing"): result(1, {"event": "error", "code": "not_found", "status": 404})}
        with unittest.mock.patch.object(boat, "boat_json", self.listing()):
            self.provider(answers=answers).create()
        self.assertEqual(boat.read(boat.POOL), [])
        self.assertEqual([(entry["id"], entry.get("retiring")) for entry in boat.read(boat.RETIRING)], [("bx_failed", True)])
        with boat.pool() as entries:
            self.assertEqual(boat.pick(entries, "bake-a", 1), [])
        boat.registry("run1").unlink()
        # Up again and of a kept bake, it is still retried because it is retiring.
        with unittest.mock.patch.object(boat, "boat_json", self.listing(bx_failed="ready")):
            self.provider().create()
        self.assertEqual(self.calls, [["delete", "bx_failed", "--yes"]])
        self.assertEqual((boat.read(boat.POOL), boat.read(boat.RETIRING)), ([], []))

    def test_reap_keeps_a_sandbox_it_failed_to_delete_in_the_pool(self):
        boat.write(boat.BAKES, ["bake-a"])
        boat.write(boat.POOL, [parked("bx_a", 1), parked("bx_b", 2)])
        failed = subprocess.CompletedProcess([], 1, json.dumps({"event": "error", "code": "internal", "status": 500}).encode(), b"")
        with unittest.mock.patch.object(boat, "boat_json", self.listing(bx_a="ready", bx_b="ready")), \
                unittest.mock.patch.object(boat.subprocess, "run", lambda args, **kwargs: failed if "bx_a" in args else result(0, {})):
            rows = boat.reap_pool(True, True)
        self.assertEqual([(row["id"], row["action"]) for row in rows], [("bx_a", "failed"), ("bx_b", "removed")])
        self.assertEqual(boat.read(boat.POOL), [])
        self.assertEqual([entry["id"] for entry in boat.read(boat.RETIRING)], ["bx_a"])

    def test_a_retiring_sandbox_never_shows_in_the_ledger_a_checkout_without_retiring_claims_from(self):
        boat.write(boat.BAKES, ["bake-a"])
        # A ledger an earlier version of this branch wrote, with the flag inline.
        boat.write(boat.POOL, [parked("bx_a", 1), dict(parked("bx_old", 2), retiring=True)])
        seen = []

        def delete(args, **kwargs):
            # A checkout that reads only POOL, as main's claim does, while the delete is in flight.
            seen.append([entry["id"] for entry in boat.read(boat.POOL)])
            return result(1, {"event": "error", "code": "internal", "status": 500})
        with unittest.mock.patch.object(boat, "boat_json", self.listing(bx_a="ready", bx_old="ready")), \
                unittest.mock.patch.object(boat.subprocess, "run", delete):
            boat.reap_pool(True, True)
        self.assertEqual(seen, [[], []])
        self.assertEqual(boat.read(boat.POOL), [])
        self.assertEqual(sorted(entry["id"] for entry in boat.read(boat.RETIRING)), ["bx_a", "bx_old"])

    def test_the_first_fresh_clone_saves_its_fingerprint_and_keeps_the_bakes_other_keys(self):
        boat.write(boat.BAKES, ["bake-a"])
        # A sibling checkout with another FINGERPRINT or NORMALIZE shares the bake and keeps its own key.
        kept = [boat.REGISTRY / name for name in ["bake-a.fingerprint", "bake-a.0123456789.fingerprint", "bake-a.notes"]]
        for path in kept:
            path.write_text("{}")
        provider = self.provider("machine-1")
        boat.write(provider.registry, dict(run="run1", sandboxes={}))
        provider.run["corrosion"] = "corrosion"
        provider.launch = lambda node: provider.mark(node["name"], id="bx_a", state="running") or "bx_a"
        info = dict(sandbox=dict(ip="2001:db8:0:1::1", sshEndpoint="203.0.113.1:22"))
        provider.boat = lambda *args, **kwargs: subprocess.CompletedProcess([], 0, json.dumps(info).encode(), b"")
        provider.wait_agent = provider.exec = lambda *args, **kwargs: None
        provider.fingerprint = lambda node: {"containers": []}
        provider.start(dict(name="machine-1"))
        self.assertEqual(boat.read(boat.fingerprints("bake-a")), {"containers": []})
        self.assertEqual([path.exists() for path in kept], [True, True, True])

    def test_the_ledger_keeps_what_one_lock_saves_and_is_no_run_record(self):
        with boat.pool() as entries:
            entries.append(parked("bx_a", 1, at=5))
        with boat.pool() as entries:
            self.assertEqual(entries, [parked("bx_a", 1, at=5)])
            entries.clear()
        self.assertEqual(boat.read(boat.POOL), [])
        with boat.pool() as entries:
            entries.append(parked("bx_b", 2))
        self.assertEqual(list(boat.REGISTRY.glob("*.json")), [])
        self.assertEqual(boat.pooled(), {"bake-a": 1})
        self.assertEqual(boat.images_in_use(), {"bake-a"})

    def test_preflight_budgets_only_the_machines_the_pool_cannot_cover(self):
        boat.write(boat.BAKES, ["bake-a", "bake-b"])
        boat.write(boat.POOL, [parked("bx_a", 1), parked("bx_b", 2), parked("bx_c", 2), parked("bx_d", 3, image="bake-b")])
        answers = dict(list=dict(sandboxes=[dict(id=sandbox, state="ready") for sandbox in ["bx_a", "bx_b", "bx_c", "bx_d"]]),
                       limits=dict(maxActiveSandboxes=100, activeSandboxes=0, starts=dict(
                           minute=dict(remaining=1), hour=dict(remaining=60), day=dict(remaining=200), unlimited=False)))
        with unittest.mock.patch.object(boat, "boat_json", lambda *args: answers[args[0]]), \
                unittest.mock.patch.object(boat, "corrosion_image", lambda core: "corrosion"), \
                unittest.mock.patch.object(boat, "bake_name", lambda corrosion: "bake-a"):
            boat.preflight(3, Path("."))
            with self.assertRaisesRegex(RuntimeError, "Starts this minute: need up to 2, 1 left"):
                boat.preflight(4, Path("."))
            with unittest.mock.patch.object(boat, "POOL_SIZE", 0), self.assertRaisesRegex(RuntimeError, "need up to 3, 1 left"):
                boat.preflight(3, Path("."))

    def test_reap_takes_stopped_gone_and_retired_bakes_and_all_under_force(self):
        entries = [parked("bx_up", 1), parked("bx_stopped", 2), parked("bx_gone", 3), parked("bx_old", 4, image="bake-c"),
                   parked("bx_sibling", 5, image="bake-b")]
        states = {"bx_up": "ready", "bx_stopped": "stopped", "bx_old": "idle", "bx_sibling": "idle"}
        found = [(entry["id"], reason) for entry, reason in boat.stale(entries, states, {"bake-a", "bake-b"})]
        self.assertEqual(found, [("bx_stopped", "stopped"), ("bx_gone", "gone"), ("bx_old", "retired bake")])
        found = [entry["id"] for entry, reason in boat.stale(entries, states, {"bake-a", "bake-b"}, True)]
        self.assertEqual(found, ["bx_up", "bx_stopped", "bx_gone", "bx_old", "bx_sibling"])

    def test_a_bake_the_ledger_or_any_run_still_uses_is_kept(self):
        boat.write(boat.BAKES, ["bake-a", "bake-b"])
        boat.write(boat.registry("run9"), dict(run="run9", image="bake-z", sandboxes={}))
        self.assertEqual(boat.kept_bakes(), {"bake-a", "bake-b", "bake-z"})

    def test_reap_reads_sandbox_states_under_the_pool_lock_and_keeps_a_sibling_bake(self):
        boat.write(boat.BAKES, ["bake-a", "bake-b"])
        boat.write(boat.POOL, [parked("bx_a", 1), parked("bx_b", 2, image="bake-b"), parked("bx_c", 3, image="bake-c")])
        held = []

        def listing(*args):
            held.append(self.locked())
            return dict(sandboxes=[dict(id=sandbox, state="ready") for sandbox in ["bx_a", "bx_b", "bx_c"]])
        with unittest.mock.patch.object(boat, "boat_json", listing):
            rows = boat.reap_pool(False, False)
        self.assertEqual(held, [True])
        self.assertEqual([(row["id"], row["reason"]) for row in rows], [("bx_c", "retired bake")])

# Docker as far as the image reset reaches it, backed by a JSON file of tags, image ids and build cache entries.
FAKE_GUEST = r"""#!/usr/bin/env python3
import json, os, sys
path = os.environ["FAKE_GUEST_STATE"]
state = json.load(open(path))
tags, ids, args = state["tags"], state["ids"], sys.argv[1:]
containers, volumes = state.setdefault("containers", []), state.setdefault("volumes", [])
networks, zpools = state.setdefault("networks", ["bridge", "host", "none"]), state.setdefault("zpools", [])
links = state.setdefault("links", ["lo"])
def drop(image):
    for tag in [t for t, i in tags.items() if i == image]:
        del tags[tag]
    ids.remove(image)
def docker():
    if args[:2] == ["image", "ls"] and "-q" in args:
        print("\n".join(ids))
    elif args[:2] == ["image", "ls"]:
        print("\n".join([f"{t} {i}" for t, i in tags.items()] + [f"<none>:<none> {i}" for i in ids if i not in tags.values()]))
    elif args[0] == "tag":
        assert args[1] in ids, f"no image {args[1]}"
        tags[args[2]] = args[1]
    elif args[:2] == ["rmi", "-f"]:
        for name in args[2:]:
            if name in tags:
                image = tags.pop(name)
                if image not in tags.values():
                    ids.remove(image)
            elif name in ids:
                drop(name)
            else:
                sys.exit(f"no such image {name}")
    elif args[:3] == ["builder", "prune", "-af"]:
        state["cache"] = 0
    elif args[:2] == ["ps", "-aq"]:
        hold = "--filter" in args
        print("\n".join(c["id"] for c in containers if c["hold"] or not hold))
    elif args[:3] == ["ps", "-a", "--format"]:
        print("\n".join(f"{c['id']}|{c['image']}|exited|{'1' if c['hold'] else ''}" for c in containers))
    elif args[:2] == ["rm", "-f"]:
        containers[:] = [c for c in containers if c["id"] not in args[2:]]
    elif args[:3] == ["volume", "ls", "-q"]:
        print("\n".join(volumes))
    elif args[:3] == ["volume", "ls", "--format"]:
        print("\n".join(f"local {v}" for v in volumes))
    elif args[:3] == ["volume", "rm", "-f"]:
        volumes[:] = [v for v in volumes if v not in args[3:]]
    elif args[:3] == ["network", "ls", "--format"]:
        print("\n".join(n + (" bridge" if "Driver" in args[3] else "") for n in networks))
    elif args[:2] == ["network", "rm"]:
        networks[:] = [n for n in networks if n not in args[2:]]
    elif args[:2] == ["info", "-f"]:
        print(os.environ["FAKE_DOCKER_ROOT"])
    elif args[:3] == ["system", "df", "--format"]:
        print(f"Images {len(ids)}\nContainers {len(containers)}\nLocal Volumes {len(volumes)}\nBuild Cache {state['cache']}")
    else:
        sys.exit(f"unexpected docker {args}")
def zpool():
    if args == ["list", "-H", "-o", "name"]:
        print("\n".join(zpools))
    elif args[:2] == ["destroy", "-f"]:
        zpools.remove(args[2])
    else:
        sys.exit(f"unexpected zpool {args}")
def ip():
    if args == ["-br", "link"]:
        print("\n".join(f"{link} UP" for link in links))
    elif args == ["-o", "link", "show"]:
        print("\n".join(f"{n}: {link}: <UP> mtu 1500" for n, link in enumerate(links, 1)))
    elif args[:2] == ["link", "del"]:
        links.remove(args[2])
    else:
        sys.exit(f"unexpected ip {args}")
def systemctl():
    if args[0] == "list-units":
        print("ployz.service loaded active running Ployz")
    elif args[0] == "list-unit-files":
        print("docker.service enabled enabled" if "--state=enabled" in args else "ployz.service enabled enabled")
    elif args[0] not in ("list-jobs", "unmask", "stop", "start"):
        sys.exit(f"unexpected systemctl {args}")
command = os.path.basename(sys.argv[0])
if command == "journalctl":
    assert args == ["--rotate"], args
elif command not in ("ss", "sleep"):
    globals()[command]()
# Scripts run reads side by side, as in comm's process substitution, so a write must never show a reader a torn file.
if state != json.load(open(path)):
    json.dump(state, open(path + ".next", "w"))
    os.replace(path + ".next", path)
"""
GUEST_COMMANDS = ["docker", "zpool", "ip", "systemctl", "ss", "sleep", "journalctl"]


class Reset(unittest.TestCase):
    """The stash and restore shell, run against a scratch tree that stands in for the guest's /."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root, self.stash = Path(directory.name, "guest"), Path(directory.name, "bake.tar")
        files = {"usr/local/bin/ployz": "bake ployz", "usr/bin/runc": "bake runc", "usr/bin/runcon": "runcon",
                 "etc/systemd/system/ployz.service": "unit", "var/lib/ployz/state": "bake state", "run/keep": "",
                 "opt/keep": "", "root/.ssh/authorized_keys": "key", "root/notes": "bake", "srv/keep": "",
                 "var/tmp/cloud-init/keep": ""}
        for name, content in files.items():
            Path(self.root, name).parent.mkdir(parents=True, exist_ok=True)
            Path(self.root, name).write_text(content)

    def sh(self, script, *args):
        script = script.replace("cd /", f"cd {self.root}").replace(boat.STASH, str(self.stash))
        subprocess.run(["bash", "-ec", script, "sh", *args], cwd=self.root, check=True, capture_output=True)

    def tree(self):
        return {str(path.relative_to(self.root)): path.read_text() if path.is_file() else "/" for path in self.root.rglob("*")}

    def write(self, name, content=""):
        Path(self.root, name).parent.mkdir(parents=True, exist_ok=True)
        Path(self.root, name).write_text(content)

    def fakes(self, state):
        """An environment whose guest commands are fakes over the `state` file; returns it and the Docker root dir."""
        bin_dir, docker_root = Path(self.root.parent, "fakes"), Path(self.root.parent, "docker-root")
        bin_dir.mkdir()
        Path(bin_dir, "guest").write_text(FAKE_GUEST)
        Path(bin_dir, "guest").chmod(0o755)
        for command in GUEST_COMMANDS:
            Path(bin_dir, command).symlink_to("guest")
        env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "FAKE_GUEST_STATE": str(state),
               "FAKE_DOCKER_ROOT": str(docker_root)}
        return env, docker_root

    def guest(self, script, env, *args, flags="-ec"):
        """Run a guest script as Provider does, with / as the scratch tree; returns its output."""
        script = script.replace(boat.STASH, str(self.stash))
        script = re.sub(r"(?<=[\s\"'(])/(?=(var|run|opt|usr|etc|root)\b)", f"{self.root}/", script)
        script = script.replace("cd / ", f"cd {self.root} ").replace("cd /\n", f"cd {self.root}\n")
        left = script.replace(str(self.root), "ROOT")
        assert not re.search(r"cd /[\s;]|[\s\"'(]/(var|run|opt|usr|etc|root)\b", left), "a guest path escapes the scratch tree"
        return subprocess.run(["bash", flags, script, "sh", *args], cwd=self.root, env=env, check=True,
                              capture_output=True, text=True).stdout

    def baked(self):
        """A bake's guest: its install stashed, its images recorded, and its fingerprint; returns env, state and fingerprint."""
        state = Path(self.root.parent, "guest.json")
        holds = [dict(id=f"hold{n}", image=f"image{n}", hold=True) for n in range(len(boat.IMAGES) + 2)]
        state.write_text(json.dumps({"tags": {"caddy:2": "sha256:aaa", "nginx:1.29-alpine": "sha256:bbb"},
                                     "ids": ["sha256:aaa", "sha256:bbb"], "cache": 0, "containers": holds}))
        self.write("root/.ssh/authorized_keys", "key\n")
        Path(self.root, "run/systemd/system").mkdir(parents=True)
        env, _ = self.fakes(state)
        self.guest(boat.KEEP_INSTALL + boat.RECORD_IMAGES, env)
        return env, state, self.fingerprint(env)

    def fingerprint(self, env):
        # Provider.fingerprint runs it without -e: a find over a path the guest lacks fails.
        return boat.fingerprint(self.guest(boat.FINGERPRINT, env, flags="-c"))

    def test_reset_returns_a_used_guest_to_the_bakes_files_docker_state_and_fingerprint(self):
        env, state, bake = self.baked()
        files, docker = self.tree(), json.loads(state.read_text())
        self.write("usr/local/bin/ployz", "run ployz")
        self.write("usr/bin/runc", "wrapper")
        self.write("var/lib/ployz/run-state", "run")
        self.write("run/systemd/system/ployz-mirror.service", "unit")
        self.write("root/.ssh/authorized_keys", "other key\n")
        self.write("srv/data", "run")
        used = json.loads(state.read_text())
        used["containers"].append(dict(id="app", image="nginx:latest", hold=False))
        used["tags"].update({"caddy:2": "sha256:ccc", "nginx:latest": "sha256:ddd"})
        used["ids"] += ["sha256:ccc", "sha256:ddd"]
        used.update(cache=3, volumes=["ployz-data"], networks=["bridge", "host", "none", "ployz"], zpools=["ployz-pool"],
                    links=["lo", "ployz0"])
        state.write_text(json.dumps(used))
        Path(env["FAKE_DOCKER_ROOT"], "ployz-observe").mkdir(parents=True)
        self.assertTrue(boat.differences(bake, self.fingerprint(env)))
        self.guest(boat.RESET, env, str(self.stash), "key")
        self.assertEqual(self.tree(), files)
        self.assertEqual(json.loads(state.read_text()), docker)
        self.assertFalse(Path(env["FAKE_DOCKER_ROOT"]).exists() and any(Path(env["FAKE_DOCKER_ROOT"]).iterdir()))
        self.assertEqual(boat.differences(bake, self.fingerprint(env)), [])

    def test_the_fingerprint_tells_a_moved_tag_a_build_cache_and_a_replaced_runc_from_the_bake(self):
        env, state, bake = self.baked()
        clean = state.read_text()
        for section, change in [("images", lambda guest: guest["tags"].update({"caddy:2": "sha256:bbb"})),
                                ("build-cache", lambda guest: guest.update(cache=2)),
                                ("replaced", lambda guest: self.write("usr/bin/runc", "wrapper"))]:
            with self.subTest(section=section):
                guest = json.loads(clean)
                change(guest)
                state.write_text(json.dumps(guest))
                found = boat.differences(bake, self.fingerprint(env))
                self.write("usr/bin/runc", "bake runc")
                self.assertEqual([line.split(" ")[0] for line in found], [section])

    def test_restore_puts_back_the_bakes_runc_and_install_and_drops_a_wrapper_and_its_original(self):
        self.sh(boat.KEEP_INSTALL)
        bake = self.tree()
        self.write("usr/local/bin/ployz", "run ployz")
        Path(self.root, "usr/bin/runc").rename(Path(self.root, "usr/bin/runc.slow-original"))
        self.write("usr/bin/runc", "wrapper")
        self.write("usr/bin/runc.fast-wrapper", "wrapper")
        self.write("var/lib/ployz/run-state", "run")
        self.write("opt/ployz-verify/bin", "run")
        self.sh("stash=$1\n" + boat.RESTORE, self.stash)
        self.assertEqual(self.tree(), bake)

    def test_restore_drops_scratch_a_run_left_and_keeps_live_agent_and_systemd_state(self):
        self.write("var/tmp/ariana/db", "agent")
        self.sh(boat.KEEP_INSTALL)
        self.write("etc/default/ployz", "PLOYZ_FLAG=1")
        self.write("root/.cache/ployz/deep/file", "run")
        self.write("root/notes", "edited")
        self.write("srv/data", "run")
        self.write("var/tmp/cloud-init/run-file", "run")
        self.write("var/tmp/a name with\nnewline", "run")
        survivors = {"var/tmp/ariana/db-wal": "agent", "var/tmp/systemd-private-abc/tmp/x": "boot"}
        for name, content in survivors.items():
            self.write(name, content)
        expected = self.tree()
        for name in ["etc/default/ployz", "root/.cache", "root/.cache/ployz", "root/.cache/ployz/deep",
                     "root/.cache/ployz/deep/file", "srv/data", "var/tmp/cloud-init/run-file", "var/tmp/a name with\nnewline"]:
            del expected[name]
        self.sh("stash=$1\n" + boat.RESTORE, self.stash)
        # Reset keeps a bake file's path; it only proves the path list, and the fingerprint lists paths, not contents.
        self.assertEqual(self.tree(), expected)

    def test_image_reset_puts_back_moved_tags_drops_new_images_and_tags_and_empties_the_build_cache(self):
        state = Path(self.root.parent, "docker.json")
        bake = {"tags": {"ployz/agent:v1": "sha256:aaa", "busybox:latest": "sha256:bbb"},
                "ids": ["sha256:aaa", "sha256:bbb", "sha256:ccc"], "cache": 0}
        state.write_text(json.dumps(bake))
        env, _ = self.fakes(state)
        run = lambda script: subprocess.run(["bash", "-ec", script, "sh", str(self.stash)], env=env, check=True,
                                            capture_output=True, text=True)
        run(f"{boat.IMAGE_TAGS} > \"$1.tags\"")
        # A run rebuilt the agent under the bake's tag, tagged the bake's old image anew, pulled one more and built.
        state.write_text(json.dumps({"tags": {"ployz/agent:v1": "sha256:ddd", "local/agent:old": "sha256:aaa",
                                              "busybox:latest": "sha256:bbb", "nginx:latest": "sha256:eee"},
                                     "ids": ["sha256:aaa", "sha256:bbb", "sha256:ccc", "sha256:ddd", "sha256:eee"],
                                     "cache": 7}))
        run("stash=$1\n" + boat.IMAGE_RESET)
        self.assertEqual({name: json.loads(state.read_text())[name] for name in bake}, bake)

    def test_journal_reset_drops_the_runs_archives_and_other_machines_and_keeps_the_open_journal(self):
        bin_dir = Path(self.root.parent, "bin")
        bin_dir.mkdir()
        # journalctl --rotate archives each open file under a name with an @ and opens a fresh one.
        Path(bin_dir, "journalctl").write_text("#!/bin/sh\n[ \"$1\" = --rotate ] || exit 1\n"
                                               "for f in var/log/journal/*/*.journal; do case $f in *@*) ;; *)\n"
                                               "mv \"$f\" \"${f%.journal}@0001.journal\"; : > \"$f\" ;; esac; done\n")
        Path(bin_dir, "journalctl").chmod(0o755)
        self.write("etc/machine-id", "abc\n")
        self.write("var/log/journal/abc/system.journal", "run")
        self.write("var/log/journal/abc/system@0000.journal", "older run")
        self.write("var/log/journal/abc/user-1000@0000.journal~", "torn")
        self.write("var/log/journal/old/system.journal", "bake's builder")
        env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}
        subprocess.run(["bash", "-ec", boat.JOURNAL_RESET], cwd=self.root, env=env, check=True, capture_output=True)
        journal = {name: content for name, content in self.tree().items() if name.startswith("var/log/journal")}
        self.assertEqual(journal, {"var/log/journal": "/", "var/log/journal/abc": "/", "var/log/journal/abc/system.journal": ""})
        # Reset runs it after RESTORE, from /.
        self.assertGreater(boat.RESET.find(boat.JOURNAL_RESET), boat.RESET.find("cd /\n" + boat.RESTORE))


if __name__ == "__main__":
    unittest.main()
