"""Incus VM backend for verify-cluster's disposable-Machine contract."""

import fcntl
import hashlib
import os
from pathlib import Path
import re
import releases
import sys
import time


class Provider:
    def __init__(self, run, commands, core):
        self.run, self.commands, self.core = run, commands, core
        self.project = "ployz-verify-" + run["id"]
        self.bridge = "pv" + run["id"]
        self.pool = (run["provider_state"].setdefault("storage_pool", os.environ.get("PLOYZ_VERIFY_INCUS_POOL", "ployz-verify"))
                     if "provider_state" in run else "ployz-verify")
        self.prefix = ([] if os.geteuid() == 0 else ["sudo", "-n"]) + ["incus", "--force-local", "--quiet"]

    def incus(self, *args, project=None, **kwargs):
        return self.commands([*self.prefix, "--project", project or self.project, *args], **kwargs)

    def create(self):
        self.incus("project", "create", self.project, "-c", "features.images=false", "-c", "features.networks=false",
                   "-c", "user.ployz.verify.run=" + self.run["id"], project="default")
        self.incus("network", "create", self.bridge, "ipv4.address=auto", "ipv4.nat=true", "ipv6.address=none",
                   "user.ployz.verify.run=" + self.run["id"], project="default")
        self.firewall(add=True)

    def firewall(self, *, add):
        for direction in ["-i", "-o"]:
            rule = ["DOCKER-USER", direction, self.bridge, "-m", "comment", "--comment", self.project, "-j", "ACCEPT"]
            if self.commands(["sudo", "-n", "iptables", "-S", "DOCKER-USER"], check=False).returncode:
                return
            exists = not self.commands(["sudo", "-n", "iptables", "-C", *rule], check=False).returncode
            if add and not exists:
                self.commands(["sudo", "-n", "iptables", "-I", rule[0], "1", *rule[1:]])
            if not add and exists:
                self.commands(["sudo", "-n", "iptables", "-D", *rule])

    def start(self, node):
        self.incus("launch", self.run["image"], node["name"], "--vm", "--no-profiles", "-n", self.bridge,
                   "-s", self.pool, "-d", "root,size=24GiB", "-c", "limits.cpu=2", "-c", "limits.memory=2GiB",
                   "-c", "security.secureboot=false")
        self.wait_agent(node)

    def power_off(self, node):
        # --force cuts power: no shutdown hooks, no flushed page cache.
        if self.state(node) != "STOPPED":
            self.incus("stop", node["name"], "--force")

    def power_on(self, node):
        if self.state(node) != "RUNNING":
            self.incus("start", node["name"])
        self.wait_agent(node)

    def state(self, node):
        return self.incus("list", node["name"], "-c", "s", "-f", "csv").stdout.decode().strip()

    def wait_agent(self, node):
        deadline = time.monotonic() + 120
        while True:
            if not self.exec(node, ["true"], check=False, timeout=10).returncode:
                return
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{node['name']}: Incus guest agent did not become ready")
            time.sleep(1)

    def exec(self, node, args, **kwargs):
        return self.incus("exec", node["name"], "-T", "--", *args, **kwargs)

    def push(self, node, source, destination):
        self.incus("file", "push", "--create-dirs", "--mode=0755", "--uid=0", "--gid=0", source, node["name"] + destination)

    def address(self, node):
        address = self.exec(node, ["sh", "-ec", "ip -4 route get 1.1.1.1 | sed -n 's/.* src \\([^ ]*\\).*/\\1/p'"]).stdout.decode().strip()
        if not address:
            raise RuntimeError(f"{node['name']}: guest has no IPv4 address")
        return address

    def destroy(self):
        # Verify ownership on the server as well as in the local manifest.
        self.incus("info", project="default")
        if not self.incus("project", "show", self.project, project="default", check=False).returncode:
            owner = self.incus("project", "get", self.project, "user.ployz.verify.run", project="default").stdout.decode().strip()
            if owner != self.run["id"]:
                raise RuntimeError(f"Refusing to delete another run's project: {self.project}")
            for node in self.run["machines"]:
                if not self.incus("info", node["name"], check=False).returncode:
                    self.incus("delete", node["name"], "--force")
            self.incus("project", "delete", self.project, project="default")
        if not self.incus("network", "show", self.bridge, project="default", check=False).returncode:
            owner = self.incus("network", "get", self.bridge, "user.ployz.verify.run", project="default").stdout.decode().strip()
            if owner != self.run["id"]:
                raise RuntimeError(f"Refusing to delete another run's bridge: {self.bridge}")
            self.incus("network", "delete", self.bridge, project="default")
        self.firewall(add=False)

    def prepare(self):
        guest = self.core / "scripts" / "verify" / "prepare-guest.sh"
        corrosion = re.search(r'pub const IMAGE: &str = "([^"]+)"',
                              (self.core / "crates/ployzd/src/corrosion/service.rs").read_text()).group(1)
        release = self.run["bootstrap_release"]
        fingerprint = hashlib.sha256(guest.read_bytes() + corrosion.encode() + release.encode() + b"published").hexdigest()[:12]
        self.run["image"] = "ployz-verify-" + fingerprint
        # One shared prepared image per recipe; simultaneous runs don't bake it twice.
        cache = Path.home() / ".cache" / "ployz" / "verify"
        cache.mkdir(parents=True, exist_ok=True)
        with (cache / "incus-image.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.incus("info", project="default")
            if self.incus("storage", "show", self.pool, project="default", check=False).returncode:
                if self.pool != "ployz-verify":
                    raise RuntimeError(f"Create the selected Incus storage pool first: {self.pool}")
                self.incus("storage", "create", self.pool, "dir", project="default")
            if not self.incus("image", "info", self.run["image"], project="default", check=False).returncode:
                return
            print("Preparing reusable Incus image (first run only).", file=sys.stderr)
            node = self.run["machines"][0]
            image = self.run["image"]
            try:
                self.create()
                self.run["image"] = "images:ubuntu/24.04"
                self.start(node)
                self.run["image"] = image
                # Both modes share a published base; checkout runs replace it
                # after cloning, so a daemon edit does not rebake dependencies.
                artifacts = Path(self.run["manifest"]).parent / "artifacts" / "bootstrap"
                releases.acquire(release, self.commands, artifacts)
                self.push(node, artifacts / "ployzd", "/opt/ployz-verify/ployzd")
                self.push(node, guest, "/opt/ployz-verify/prepare-guest.sh")
                self.exec(node, ["bash", "/opt/ployz-verify/prepare-guest.sh", release, corrosion], timeout=1200)
                self.incus("stop", node["name"], "--timeout", "30")
                self.incus("publish", node["name"], "--alias", image, "--compression", "none", timeout=300)
            finally:
                self.run["image"] = image
                self.destroy()
