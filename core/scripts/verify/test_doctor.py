#!/usr/bin/env python3
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

sys.dont_write_bytecode = True
scripts = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("verify_cluster", str(scripts / "verify-cluster"))
cluster = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
loader.exec_module(cluster)

DRAINING = "a" * 64
FORCED = f" WARN forcing retire after 300 s; never acked: [] container={DRAINING} service=app/web"
KEPT = f" INFO kept retire: a pointer serves it or no longer retires it container={DRAINING}"


class Journal:
    def __init__(self, lines):
        self.lines = lines

    def exec(self, node, command):
        return subprocess.CompletedProcess(command, 0, "".join(line + "\n" for line in self.lines).encode())


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
        self.assertEqual(self.overdue(FORCED),
                         ["machine-1: web Container aaaaaaaaaaaa still drains past 300 s; see journalctl -u ployz"])

    def test_a_kept_episode_does_not_fail_a_fresh_retirement(self):
        self.assertEqual(self.overdue(FORCED, KEPT), [])

    def test_a_retirement_forced_again_after_a_kept_episode_fails(self):
        self.assertEqual(len(self.overdue(FORCED, KEPT, FORCED)), 1)


if __name__ == "__main__":
    unittest.main()
