#!/usr/bin/env python3
"""Helper commands must not wait on the caller's stdin: `incus launch` reads YAML from a non-terminal one."""

import importlib.machinery
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest

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


if __name__ == "__main__":
    unittest.main()
