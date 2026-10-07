"""Prove that an interrupted Cloud Deploy finishes through a replacement worker."""

import json
import os
from pathlib import Path
import time


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def recover_deploy(manifest, deploy, number, verify, cli):
    run = json.loads(manifest.read_text())
    pid_file = Path(run["cloud"]["dashboard"]) / ".verify" / "run" / "worker.pid"

    def newest():
        return json.loads(cli("--json", "deployment", "ls"))["deployments"][0]

    deadline = time.monotonic() + 120
    while True:
        deployment = newest()
        if deployment["number"] == number and deployment["status"] == "running":
            break
        assert time.monotonic() < deadline, f"The Deploy never started running: {deployment}"
        time.sleep(0.5)

    interrupted = deployment["id"]
    old_pid = int(pid_file.read_text())
    assert alive(old_pid), f"The recorded worker {old_pid} was not alive before runner kill"
    verify("runner", "kill", manifest)
    assert not alive(old_pid), f"The worker {old_pid} survived runner kill"
    time.sleep(10)
    deployment = newest()
    assert deploy.poll() is None and deployment["id"] == interrupted and deployment["status"] == "running", \
        f"The interrupted Deploy settled without its runner: {deployment}"

    verify("runner", "start", manifest)
    new_pid = int(pid_file.read_text())
    assert new_pid != old_pid and alive(new_pid), f"runner start did not replace worker {old_pid}: {new_pid}"
    output = deploy.communicate(timeout=900)[0]
    assert deploy.returncode == 0 and "of verify/production: applied" in output, f"The Deploy did not complete:\n{output}"
    deployment = newest()
    assert deployment["id"] == interrupted and deployment["number"] == number and deployment["status"] == "applied", \
        f"The interrupted Deploy did not apply: {deployment}"
    assert not alive(old_pid) and alive(new_pid), f"The replacement worker {new_pid} did not survive recovery"
    web = [container for container in json.loads(cli("--json", "ps"))["containers"]
           if container["labels"].get("ployz.service.name") == "web"]
    assert len(web) == 1, f"Expected one web container after the resumed Deploy: {web}"
    return dict(deployment_id=interrupted, number=number, old_worker_pid=old_pid, new_worker_pid=new_pid)
