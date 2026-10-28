#!/usr/bin/env python3
"""Helper commands must not wait on the caller's stdin: `incus launch` reads YAML from a non-terminal one."""

import importlib.machinery
import importlib.util
import os
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
