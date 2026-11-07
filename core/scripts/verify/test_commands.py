#!/usr/bin/env python3
"""Helper commands must not wait on the caller's stdin: `incus launch` reads YAML from a non-terminal one."""

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import unittest
import unittest.mock

sys.dont_write_bytecode = True
scripts = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("verify_cluster", str(scripts / "verify-cluster"))
cluster = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
loader.exec_module(cluster)


class OpenStdin(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.commands = cluster.Commands(dict(manifest=str(Path(directory.name) / "cluster.json")))
        # An agent shell hands its children a pipe that never reaches EOF.
        read, write = os.pipe()
        saved = os.dup(0)
        os.dup2(read, 0)
        os.close(read)
        self.addCleanup(os.close, write)
        self.addCleanup(os.close, saved)
        self.addCleanup(os.dup2, saved, 0)

    def test_a_command_that_reads_stdin_sees_eof(self):
        result = self.commands(["cat"], timeout=5)
        self.assertEqual((result.returncode, result.stdout), (0, b""))

    def test_a_caller_can_still_send_input(self):
        self.assertEqual(self.commands(["cat"], input=b"yaml", timeout=5).stdout, b"yaml")


class Enrollment(unittest.TestCase):
    """`--json server add` prints its failure on stdout and leaves stderr empty."""

    def test_a_refused_enrollment_reports_the_printed_error(self):
        error = b'{"error":{"code":"conflict","message":"This Organization\'s Servers can\'t be reached."}}\n'
        result = cluster.subprocess.CompletedProcess([], 1, error, b"")
        with self.assertRaises(RuntimeError) as raised:
            cluster.enrolled(dict(name="machine-1"), result, "/evidence")
        self.assertIn("machine-1: enrollment failed:", str(raised.exception))
        self.assertIn("This Organization's Servers can't be reached.", str(raised.exception))

    def test_a_committed_enrollment_with_an_unavailable_follow_up_is_returned(self):
        response = dict(server=dict(machine=dict(id="m1")), follow_up_error=dict(code="unavailable"))
        result = cluster.subprocess.CompletedProcess([], 3, cluster.json.dumps(response).encode(), b"")
        self.assertEqual(cluster.enrolled(dict(name="machine-1"), result, "/evidence"), response)



class PublicIp(unittest.TestCase):
    """A Server's public IP is the address the internet reaches it on, or none."""

    def test_a_boat_sandbox_reports_its_routable_ipv6(self):
        self.assertEqual(cluster.public_ip("2001:41d0:8:829c:100::30"), "2001:41d0:8:829c:100::30")

    def test_a_private_address_reports_none(self):
        for address in ("10.115.0.12", "192.168.122.48", "fd00::5"):
            self.assertEqual(cluster.public_ip(address), "none", address)


class EnrollPublicIp(unittest.TestCase):
    """Enrollment gives a Boat Server its IPv6 as the public IP, through Cloud too, and leaves a private address none."""

    def enroll(self, address, cloud):
        calls = []

        def cli(run, commands, args, **kwargs):
            calls.append(args)
            added = cluster.json.dumps(dict(server=dict(machine=dict(id="m1")))).encode()
            return cluster.subprocess.CompletedProcess(args, 0, added, b"")
        run = dict(cloud=dict(url="http://localhost:3000")) if cloud else {}
        node = dict(name="machine-1", ssh="root@203.0.113.1:22", address=address, endpoint=cluster.endpoint(address))
        with unittest.mock.patch.object(cluster, "cli", cli):
            cluster.enroll(run, unittest.mock.Mock(evidence="/evidence"), node, Path("/private"))
        return calls

    def public_ip_flag(self, args):
        return args[args.index("--public-ip") + 1]

    def test_a_standalone_boat_server_enrolls_with_its_ipv6(self):
        calls = self.enroll("2001:41d0:8:829c:100::30", cloud=False)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][1:3], ["server", "add"])
        self.assertEqual(self.public_ip_flag(calls[0]), "2001:41d0:8:829c:100::30")

    def test_a_cloud_boat_server_sets_its_ipv6_after_enrolling(self):
        calls = self.enroll("2001:41d0:8:829c:100::30", cloud=True)
        self.assertEqual([args[1:3] for args in calls], [["server", "add"], ["server", "set"]])
        self.assertEqual(calls[1], ["--json", "server", "set", "m1", "--public-ip", "2001:41d0:8:829c:100::30"])

    def test_a_private_address_enrolls_with_no_public_ip_and_sets_none(self):
        for cloud in (False, True):
            calls = self.enroll("10.115.0.12", cloud=cloud)
            self.assertEqual(len(calls), 1, cloud)
            self.assertEqual(self.public_ip_flag(calls[0]), "none")


class Add(unittest.TestCase):
    """add boots and enrolls one more Machine under a name no earlier add used."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.run_ = dict(manifest=str(Path(directory.name) / "cluster.json"), provider="boat", timings={},
                         machines=[dict(name=f"machine-{index}", daemon=dict(source="checkout", version="0.4.0"))
                                   for index in (1, 2)])
        cluster.save(self.run_)
        self.backend = unittest.mock.Mock()

    def add(self, provision):
        with unittest.mock.patch.object(cluster.boat, "boat_json"), \
             unittest.mock.patch.object(cluster.boat, "shortfalls", return_value=[]), \
             unittest.mock.patch.object(cluster, "provision", side_effect=provision), \
             unittest.mock.patch.object(cluster, "enroll"), \
             unittest.mock.patch.object(cluster, "wait_ready"), \
             contextlib.redirect_stdout(io.StringIO()) as printed, contextlib.redirect_stderr(io.StringIO()):
            cluster.add(self.run_, unittest.mock.Mock(evidence="/evidence"), self.backend, None)
        return printed.getvalue()

    def test_a_failed_add_removes_its_guest_and_the_next_add_takes_a_new_name(self):
        with self.assertRaises(RuntimeError):
            self.add(RuntimeError("boat new failed"))
        self.backend.discard.assert_called_once_with("machine-3", "add failed")
        self.assertEqual([node["name"] for node in self.run_["machines"]], ["machine-1", "machine-2"])
        self.assertEqual(self.add(None), "machine-4\n")
        saved = cluster.json.loads(Path(self.run_["manifest"]).read_text())
        self.assertEqual([node["name"] for node in saved["machines"]], ["machine-1", "machine-2", "machine-4"])
        self.assertEqual(saved["machines"][2]["daemon"], self.run_["machines"][0]["daemon"])

    def test_the_new_machine_is_ready_once_it_lists_itself_up_while_a_peer_is_down(self):
        listing = cluster.json.dumps(dict(servers=[dict(machine=dict(id="m1"), membership="down"),
                                                   dict(machine=dict(id="m3"), membership="up")])).encode()
        node = dict(name="machine-3", ssh="root@203.0.113.3:22", machine_id="m3")
        with unittest.mock.patch.object(cluster, "cli", return_value=cluster.subprocess.CompletedProcess([], 3, listing, b"")):
            cluster.joined({}, unittest.mock.Mock(evidence="/evidence"), node, 1)

    def test_roles_enroll_the_server_with_only_those_roles(self):
        calls = []

        def cli(run, commands, args, **kwargs):
            calls.append(args)
            added = cluster.json.dumps(dict(server=dict(machine=dict(id="m3")))).encode()
            return cluster.subprocess.CompletedProcess(args, 0, added, b"")
        node = dict(name="machine-3", ssh="root@203.0.113.1:22", address="10.0.0.3", endpoint="10.0.0.3:51820")
        with unittest.mock.patch.object(cluster, "cli", cli):
            cluster.enroll({}, unittest.mock.Mock(evidence="/evidence"), node, Path("/private"), {"ingress"})
        self.assertEqual(calls[0][-3:], ["--accepts-builds=false", "--accepts-services=false", "--accepts-ingress=true"])


class CloudCredentials(unittest.TestCase):
    """On a --cloud run, `cli` holds the Organization Token, and --device the seeded owner's approved device."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        (root / "private").mkdir()
        (root / "private" / "cloud-token").write_text("token")
        self.run_ = dict(manifest=str(root / "cluster.json"), cloud=dict(url="http://localhost:1", organization="ada"))

    def test_the_device_cli_holds_no_token_and_its_own_config(self):
        _, environment, _ = cluster.cli_environment(self.run_, device=True)
        self.assertNotIn("PLOYZ_TOKEN", environment)
        self.assertEqual(environment["PLOYZ_CONFIG"], str(cluster.device_config(self.run_)))
        _, environment, _ = cluster.cli_environment(self.run_)
        self.assertEqual(environment["PLOYZ_TOKEN"], "token")

    def test_approving_types_the_organization_at_a_terminal(self):
        commands = unittest.mock.Mock()
        cluster.answer(self.run_, commands, "id-1", approve=True)
        args, kwargs = commands.call_args
        self.assertEqual(args[0][:2], ["script", "-qfec"])
        self.assertTrue(args[0][2].endswith("org approval approve id-1"))
        self.assertEqual(kwargs["input"], b"ada\r")


class RunState(unittest.TestCase):
    """A run that up never finished, or down never finished, accepts only down."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.manifest = Path(directory.name).resolve() / "cluster.json"

    def write(self, state):
        # A killed up leaves its Machines without address or ssh.
        cluster.save(dict(root=str(cluster.CORE), manifest=str(self.manifest), id="t1", provider="boat", state=state,
                          timings={}, machines=[dict(name="machine-1")]))

    def main(self, *argv, backend=None):
        provider = (unittest.mock.Mock(return_value=backend) if backend else
                    unittest.mock.Mock(side_effect=AssertionError("provider reached")))
        with unittest.mock.patch.object(sys, "argv", ["verify-cluster", *argv]), \
             unittest.mock.patch.object(cluster, "provider", provider):
            return cluster.main()

    def test_a_run_left_starting_or_tearing_down_refuses_every_command_but_down(self):
        manifest = str(self.manifest)
        for state in ("starting", "tearing-down"):
            self.write(state)
            for argv in (["exec", manifest, "machine-1", "--", "true"], ["cli", manifest, "--", "server", "ls"],
                         ["power", "on", manifest, "machine-1"], ["doctor", manifest], ["update", manifest]):
                with self.subTest(state=state, action=argv[0]), self.assertRaisesRegex(RuntimeError, state):
                    self.main(*argv)

    def test_down_removes_a_run_left_starting(self):
        def hung(signum, frame):
            raise TimeoutError("down hung")
        self.addCleanup(signal.signal, signal.SIGALRM, signal.signal(signal.SIGALRM, hung))
        self.addCleanup(signal.alarm, 0)
        self.write("starting")
        signal.alarm(10)
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(self.main("down", str(self.manifest), backend=unittest.mock.Mock()), 0)
        self.assertEqual(json.loads(self.manifest.read_text())["state"], "removed")

    def test_a_down_during_up_leaves_nothing_up_made(self):
        core = self.manifest.parent
        sandboxes = set()
        backend = unittest.mock.Mock()
        backend.create.side_effect = lambda: sandboxes.add("machine-1")
        backend.destroy.side_effect = sandboxes.clear
        downs = []

        def concurrent_down_then_failure(run, *args, **kwargs):
            try:
                downs.append(self.main("down", run["manifest"], backend=backend))
            except RuntimeError as error:
                downs.append(str(error))
            backend.create()
            raise RuntimeError("boot failed")
        args = cluster.argparse.Namespace(provider="boat", machines=1, memory=None, disk=None, bare=False,
                                          distro=cluster.boat.DISTROS[0], cloud=False, cloud_attach=False, daemon=None)
        with unittest.mock.patch.object(cluster, "CORE", core), \
             unittest.mock.patch.object(cluster, "preflight"), \
             unittest.mock.patch.object(cluster.host, "owner", return_value={}), \
             unittest.mock.patch.object(cluster, "provider", return_value=backend), \
             unittest.mock.patch.object(cluster, "select_daemons", side_effect=concurrent_down_then_failure), \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()), \
             self.assertRaises(RuntimeError):
            cluster.up(args)
        self.assertEqual(sandboxes, set())
        self.assertEqual(downs, ["Another up, update or teardown is using this run"])
        manifest, = (core / ".verify").glob("*/cluster.json")
        self.assertEqual(json.loads(manifest.read_text())["state"], "removed")

    def write_unknown(self, state):
        self.write(state)
        if state is None:
            run = json.loads(self.manifest.read_text())
            del run["state"]
            cluster.save(run)

    def test_a_run_with_an_unknown_state_refuses_every_command_but_down(self):
        for state, named in (("bogus", "'bogus'"), (None, "None")):
            with self.subTest(state=state):
                self.write_unknown(state)
                with self.assertRaisesRegex(RuntimeError, f"state {named} is unknown"):
                    self.main("doctor", str(self.manifest))
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(self.main("down", str(self.manifest), backend=unittest.mock.Mock()), 0)
                self.assertEqual(json.loads(self.manifest.read_text())["state"], "removed")

    def reap(self, *found, backend):
        args = cluster.argparse.Namespace(runs=[run["id"] for run in found], older_than=24, force=True, yes=True)
        row = dict(run="", age="", machines="", ram="", owner="", checkout="")
        output = io.StringIO()
        with unittest.mock.patch.object(cluster, "host_runs", return_value=list(found)), \
             unittest.mock.patch.object(cluster.host, "verdict", return_value=(True, "old")), \
             unittest.mock.patch.object(cluster, "describe", side_effect=lambda found, now: dict(row, run=found["id"])), \
             unittest.mock.patch.object(cluster, "provider", return_value=backend), \
             contextlib.redirect_stdout(output):
            cluster.reap(args)
        return {line.split()[0]: line.split()[1] for line in output.getvalue().splitlines()[1:]}

    def found(self, manifest, id="t1"):
        return dict(id=id, provider="boat", owner=dict(manifest=str(manifest)), machines=[dict(name="machine-1")])

    def test_reap_destroys_what_a_removed_run_still_holds(self):
        # A Boat key that may not delete a sandbox stops it, and teardown still ends removed.
        self.write("removed")
        backend = unittest.mock.Mock()
        self.assertEqual(self.reap(self.found(self.manifest), backend=backend), {"t1": "removed"})
        backend.destroy.assert_called_once()
        self.assertEqual(json.loads(self.manifest.read_text())["state"], "removed")

    def test_reap_removes_a_run_with_an_unknown_state(self):
        for state in ("bogus", None):
            with self.subTest(state=state):
                self.write_unknown(state)
                backend = unittest.mock.Mock()
                self.assertEqual(self.reap(self.found(self.manifest), backend=backend), {"t1": "removed"})
                backend.destroy.assert_called_once()
                self.assertEqual(json.loads(self.manifest.read_text())["state"], "removed")

    def test_one_unreadable_manifest_does_not_stop_reap(self):
        broken = self.manifest.with_name("broken.json")
        broken.write_text("{")
        self.write("ready")
        backend = unittest.mock.Mock()
        rows = self.reap(self.found(broken, id="t0"), self.found(self.manifest), backend=backend)
        self.assertEqual(rows, {"t0": "failed", "t1": "removed"})
        self.assertEqual(json.loads(self.manifest.read_text())["state"], "removed")


if __name__ == "__main__":
    unittest.main()
