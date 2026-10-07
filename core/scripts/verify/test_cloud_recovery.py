#!/usr/bin/env python3
"""Cloud recovery must kill the recorded worker and finish the same Deploy through a new one."""

import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import cloud_recovery  # noqa: E402


class SlowDeploy:
    def __init__(self, directory):
        self.manifest = directory / "cluster.json"
        self.manifest.write_text(json.dumps({"cloud": {"dashboard": str(directory)}}))
        self.pid_file = directory / ".verify" / "run" / "worker.pid"
        self.pid_file.parent.mkdir(parents=True)
        self.pid_file.write_text("41")
        self.processes = {41}
        self.now = 0
        self.returncode = None
        self.deployment = dict(id="interrupted-deploy", number=2, status="running")
        self.noop = None

    def sleep(self, seconds):
        self.now += seconds

    def verify(self, command, action, manifest):
        if action == self.noop:
            return
        if action == "kill":
            self.processes.remove(41)
        else:
            self.processes.add(42)
            self.pid_file.write_text("42")

    def cli(self, *args):
        if args == ("--json", "deployment", "ls"):
            return json.dumps({"deployments": [self.deployment]})
        return json.dumps({"containers": [{"labels": {"ployz.service.name": "web"}}]})

    def poll(self):
        return self.returncode

    def communicate(self, timeout):
        if not self.processes:
            raise subprocess.TimeoutExpired("deploy", timeout)
        self.now = 20
        self.returncode = 0
        self.deployment["status"] = "applied"
        return "of verify/production: applied", None


class Recovery(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.deploy = SlowDeploy(Path(directory.name))
        clock = SimpleNamespace(monotonic=lambda: self.deploy.now, sleep=self.deploy.sleep)
        self.clock = patch.object(cloud_recovery, "time", clock)
        self.clock.start()
        self.addCleanup(self.clock.stop)
        self.alive = patch.object(cloud_recovery, "alive", side_effect=lambda pid: pid in self.deploy.processes)
        self.alive.start()
        self.addCleanup(self.alive.stop)

    def recover(self):
        return cloud_recovery.recover_deploy(self.deploy.manifest, self.deploy, 2, self.deploy.verify, self.deploy.cli)

    def test_noop_kill_cannot_pass_a_slow_deploy(self):
        self.deploy.noop = "kill"
        with self.assertRaisesRegex(AssertionError, "41 survived runner kill"):
            self.recover()

    def test_noop_start_cannot_pass_a_slow_deploy(self):
        self.deploy.noop = "start"
        with self.assertRaisesRegex(AssertionError, "runner start did not replace worker 41"):
            self.recover()

    def test_new_worker_finishes_the_interrupted_deploy(self):
        result = self.recover()
        self.assertEqual(result, dict(deployment_id="interrupted-deploy", number=2, old_worker_pid=41, new_worker_pid=42))
        self.assertEqual(self.deploy.processes, {42})
        self.assertEqual(self.deploy.now, 20)

    def test_a_different_completed_deploy_is_not_recovery(self):
        communicate = self.deploy.communicate

        def replace(timeout):
            output = communicate(timeout)
            self.deploy.deployment["id"] = "different-deploy"
            return output

        self.deploy.communicate = replace
        with self.assertRaisesRegex(AssertionError, "interrupted Deploy did not apply"):
            self.recover()


class LocalRunner(unittest.TestCase):
    def test_real_runner_commands_replace_a_local_process(self):
        scripts = Path(__file__).resolve().parents[1]
        loader = importlib.machinery.SourceFileLoader("verify_cluster", str(scripts / "verify-cluster"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        cluster = importlib.util.module_from_spec(spec)
        loader.exec_module(cluster)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            deploy = SlowDeploy(directory)
            deploy.pid_file.unlink()
            worker = directory / "scripts" / "verify" / "worker.sh"
            worker.parent.mkdir(parents=True)
            shutil.copy2(scripts.parents[1] / "dashboard" / "scripts" / "verify" / "worker.sh", worker)
            (deploy.pid_file.parent / "env").write_text("")
            with socket.socket() as address:
                address.bind(("127.0.0.1", 0))
                (deploy.pid_file.parent / "worker.port").write_text(str(address.getsockname()[1]))
            binaries = directory / "bin"
            binaries.mkdir()
            node = binaries / "node"
            node.write_text(f"#!{sys.executable}\n" +
                            "import http.server, os\n"
                            "class Ready(http.server.BaseHTTPRequestHandler):\n"
                            "    def do_GET(self):\n"
                            "        self.send_response(200)\n"
                            "        self.end_headers()\n"
                            "    def log_message(self, *args):\n"
                            "        pass\n"
                            "server = http.server.HTTPServer(('127.0.0.1', int(os.environ['PORT'])), Ready)\n"
                            "server.serve_forever()\n")
            node.chmod(0o755)
            environment = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ["PATH"])

            def commands(args, **kwargs):
                return subprocess.run(args, env=environment, capture_output=True, check=True, **kwargs)

            def stop():
                if not deploy.pid_file.exists():
                    return
                pid = int(deploy.pid_file.read_text())
                if cluster.alive(pid):
                    os.killpg(pid, signal.SIGKILL)

            try:
                commands([worker], timeout=120)
                old_pid = int(deploy.pid_file.read_text())
                run = json.loads(deploy.manifest.read_text())

                def verify(command, action, manifest):
                    cluster.runner(run, commands, action == "start")

                clock = SimpleNamespace(monotonic=time.monotonic, sleep=lambda seconds: None)
                with patch.object(cloud_recovery, "time", clock):
                    result = cloud_recovery.recover_deploy(deploy.manifest, deploy, 2, verify, deploy.cli)
                self.assertEqual(result["old_worker_pid"], old_pid)
                self.assertNotEqual(result["new_worker_pid"], old_pid)
                self.assertFalse(cluster.alive(old_pid))
                self.assertTrue(cluster.alive(result["new_worker_pid"]))
            finally:
                stop()


if __name__ == "__main__":
    unittest.main()
