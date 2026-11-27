#!/usr/bin/env python3
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
scripts = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("verify_cluster", str(scripts / "verify-cluster"))
cluster = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
loader.exec_module(cluster)

DRAINING = "a" * 64
FORCED = f" WARN forcing retire after 300 s; never acked: [] container={DRAINING} service=app/web"
STARTED = f" INFO retire clock started; forcing after 300 s container={DRAINING} service=app/web"
FAILED = f" WARN failed to retire Container, retrying in 5 s container={DRAINING} error=refused"


class Journal:
    def __init__(self, lines):
        self.lines = lines

    def exec(self, node, command):
        with tempfile.TemporaryDirectory() as bin_dir:
            journalctl = Path(bin_dir) / "journalctl"
            journalctl.write_text("#!/bin/sh\ncat\n")
            journalctl.chmod(0o755)
            env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}
            journal = "".join(line + "\n" for line in self.lines).encode()
            return subprocess.run(command, input=journal, capture_output=True, check=True, env=env)


class OverdueDrains(unittest.TestCase):
    def setUp(self):
        self.run_ = {"machines": [{"name": "machine-1", "machine_id": "m1", "ssh": "root@machine-1"}]}
        ps = {"containers": [{"machine_id": "m1", "container_id": DRAINING, "draining": True,
                              "resolved_spec": {"name": "web"}}]}
        saved = cluster.cli
        cluster.cli = lambda run, commands, args: subprocess.CompletedProcess(args, 0, json.dumps(ps))
        self.addCleanup(setattr, cluster, "cli", saved)

    def overdue(self, *lines):
        return cluster.overdue_drains(self.run_, None, Journal(list(lines)))

    def test_a_forced_current_episode_fails(self):
        self.assertEqual(self.overdue(STARTED, FORCED),
                         ["machine-1: web Container aaaaaaaaaaaa still drains past 300 s; see journalctl -u ployz"])

    def test_a_failed_force_then_rollback_does_not_fail_a_fresh_retirement(self):
        self.assertEqual(self.overdue(STARTED, FORCED, FAILED, STARTED), [])

    def test_a_force_interrupted_by_a_restart_does_not_fail_a_fresh_retirement(self):
        self.assertEqual(self.overdue(STARTED, FORCED, STARTED), [])

    def test_a_retirement_forced_again_after_a_fresh_start_fails(self):
        self.assertEqual(len(self.overdue(STARTED, FORCED, STARTED, FORCED)), 1)

class Underlay:
    """A host network where only `cut` pairs of addresses drop each other's packets."""

    def __init__(self, cut, without_ping=False, unreachable=()):
        self.cut = {frozenset(pair) for pair in cut}
        self.without_ping = without_ping
        self.unreachable = set(unreachable)

    def exec(self, node, command, check=True, timeout=None):
        if node["name"] in self.unreachable:
            return subprocess.CompletedProcess(command, 255)
        if self.without_ping:
            return subprocess.CompletedProcess(command, 127)
        silent = frozenset((node["address"], command[-1])) in self.cut
        return subprocess.CompletedProcess(command, 1 if silent else 0)


class Reachability(unittest.TestCase):
    machines = [{"name": f"machine-{n}", "address": f"2001:db8:{n}::1", "ssh": f"root@198.51.100.{n}:19032"} for n in (1, 2, 3)]

    def require(self, cut, nodes=None, others=None, provider="boat", without_ping=False, unreachable=()):
        cluster.require_reachable({"provider": provider}, Underlay(cut, without_ping, unreachable),
                                  self.machines if nodes is None else nodes, others)

    def test_a_pair_that_cant_reach_each_other_fails_before_enrollment_naming_both_hosts(self):
        with self.assertRaises(cluster.Infrastructure) as raised:
            self.require([("2001:db8:1::1", "2001:db8:3::1")])
        message = str(raised.exception)
        self.assertIn("machine-1 (198.51.100.1) and machine-3 (198.51.100.3)", message)
        self.assertNotIn("machine-2 (", message)

    def test_a_cluster_whose_every_pair_answers_passes(self):
        self.require([])

    def test_an_added_machine_is_checked_only_against_the_others(self):
        new, *rest = self.machines
        self.require([("2001:db8:2::1", "2001:db8:3::1")], [new], rest)
        with self.assertRaises(cluster.Infrastructure):
            self.require([("2001:db8:1::1", "2001:db8:2::1")], [new], rest)

    def test_a_lone_machine_has_no_pair_to_check(self):
        self.require([], self.machines[:1])
        self.require([], self.machines[:1], [])

    def test_a_machine_without_ping_does_not_fail_the_run(self):
        self.require([], without_ping=True)

    def test_a_machine_ssh_cant_reach_is_not_reported_as_a_broken_pair(self):
        with self.assertRaises(cluster.Infrastructure) as raised:
            self.require([], unreachable=["machine-1"])
        self.assertIn("couldn't run the pair check over SSH on machine-1", str(raised.exception))
        self.assertNotIn("can't reach each other", str(raised.exception))

    def test_hosts_other_than_boat_are_not_checked(self):
        self.require([("2001:db8:1::1", "2001:db8:3::1")], provider="local")


if __name__ == "__main__":
    unittest.main()
