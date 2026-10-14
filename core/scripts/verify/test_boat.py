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

    def test_prepare_spares_a_bake_a_live_run_uses(self):
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
        with contextlib.redirect_stderr(io.StringIO()) as said:
            self.provider.refresh()
        self.assertEqual(sorted(self.calls), [["extend", "bx_a", "--ttl", boat.TTL], ["extend", "bx_b", "--ttl", boat.TTL]])
        self.assertEqual(said.getvalue().count("\n"), 1)
        self.assertIn("bx_b", said.getvalue())
        self.assertNotIn("bx_a", said.getvalue())

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


def parked(sandbox, node, image="bake-a", at=0):
    return dict(id=sandbox, image=image, ip=f"2001:db8:0:{node}::1", ssh=f"203.0.113.{node}:22", parked_at=at)


class Pool(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        for name, value in [("REGISTRY", Path(directory.name)), ("POOL", Path(directory.name) / boat.POOL.name)]:
            self.addCleanup(setattr, boat, name, getattr(boat, name))
            setattr(boat, name, value)

    def provider(self, *machines):
        self.calls = []

        def fake(args, check=True, **kwargs):
            self.calls.append(args[2:-1])
            return result(0, {})
        run = dict(id="run1", manifest=str(boat.REGISTRY / "cluster.json"), image="bake-a",
                   machines=[dict(name=name) for name in machines])
        return boat.Provider(run, fake, Path("."))

    def locked(self):
        """Whether some open file holds pool.lock now."""
        with (boat.REGISTRY / "pool.lock").open("a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return True
            fcntl.flock(lock, fcntl.LOCK_UN)
            return False

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
                   parked("bx_other", 4, image="bake-b", at=9), parked("bx_down", 5, at=8), parked("bx_taken", 6, at=7)]
        up = {"bx_old", "bx_new", "bx_two", "bx_three", "bx_other", "bx_taken"}
        picked, dead = boat.pick(entries, "bake-a", up, {"bx_taken"}, 2)
        self.assertEqual([entry["id"] for entry in picked], ["bx_new", "bx_two"])
        self.assertEqual([entry["id"] for entry in dead], ["bx_down"])
        picked, _ = boat.pick(entries, "bake-a", up, {"bx_taken"}, 5)
        self.assertEqual([entry["id"] for entry in picked], ["bx_new", "bx_two", "bx_three"])

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

    def test_reap_takes_stopped_gone_and_older_bakes_and_all_under_force(self):
        entries = [parked("bx_up", 1), parked("bx_stopped", 2), parked("bx_gone", 3), parked("bx_old", 4, image="bake-b")]
        states = {"bx_up": "ready", "bx_stopped": "stopped", "bx_old": "idle"}
        found = [(entry["id"], reason) for entry, reason in boat.stale(entries, states, "bake-a", False)]
        self.assertEqual(found, [("bx_stopped", "stopped"), ("bx_gone", "gone"), ("bx_old", "older bake")])
        found = [entry["id"] for entry, reason in boat.stale(entries, states, "bake-a", True)]
        self.assertEqual(found, ["bx_up", "bx_stopped", "bx_gone", "bx_old"])


if __name__ == "__main__":
    unittest.main()
