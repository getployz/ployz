#!/usr/bin/env python3
"""Shared-host hygiene: whether a launch fits, which runs reap removes, and who killed a VM."""

import os
from pathlib import Path
import subprocess
import sys
import shutil
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from host import GiB, killer, owner, owner_live, shortfalls, size, verdict  # noqa: E402
import incus  # noqa: E402

HOUR = 3600
DAY = 24 * HOUR


def vm(status="Running", memory=2 * GiB, rss=2 * GiB):
    return dict(status=status, memory=memory, rss=rss)


class Fit(unittest.TestCase):
    def fits(self, machines, available, running=(), pool_free=200 * GiB):
        return shortfalls(machines, 2 * GiB, available, 32 * GiB, list(running), (pool_free, 700 * GiB))

    def test_one_machine_fits_an_idle_host(self):
        self.assertEqual(self.fits(1, available=20 * GiB), [])

    def test_memory_running_guests_have_not_touched_yet_is_already_spoken_for(self):
        # 10 GiB available, but four fresh guests may still grow by 1.5 GiB each.
        fresh = [vm(rss=GiB // 2)] * 4
        self.assertEqual(self.fits(1, available=10 * GiB), [])
        problems = self.fits(1, available=10 * GiB, running=fresh)
        self.assertEqual(len(problems), 1)
        self.assertTrue(problems[0].startswith("RAM:"), problems)

    def test_stopped_guests_promise_nothing(self):
        self.assertEqual(self.fits(1, available=10 * GiB, running=[vm("Stopped", rss=0)] * 4), [])

    def test_each_machine_counts(self):
        self.assertEqual(self.fits(2, available=9 * GiB), [])
        self.assertTrue(self.fits(3, available=9 * GiB))

    def test_disk_short_is_reported_separately(self):
        problems = self.fits(2, available=20 * GiB, pool_free=40 * GiB)
        self.assertEqual([problem.split(":")[0] for problem in problems], ["Disk"])

    def test_reserve_override_simulates_a_full_host(self):
        problems = shortfalls(1, 2 * GiB, 20 * GiB, 32 * GiB, [], (200 * GiB, 700 * GiB), memory_reserve=19 * GiB)
        self.assertTrue(problems and problems[0].startswith("RAM:"))

    def test_disk_reserve_override_applies_to_the_host_disk(self):
        problems = shortfalls(1, 2 * GiB, 20 * GiB, 32 * GiB, [], (40 * GiB, 100 * GiB), (500 * GiB, 725 * GiB),
                              disk_reserve=498 * GiB)
        self.assertEqual([problem.split(":")[0] for problem in problems], ["Disk"])


class PoolCapacity(unittest.TestCase):
    def capacity(self, source, host_free, used=8 * GiB, host_total=780 * GiB):
        pool = {"/1.0/storage-pools/p/resources": dict(space=dict(total=100 * GiB, used=used)),
                "/1.0/storage-pools/p": dict(config=dict(source=source, size="100GiB"))}
        seen = []

        def disk_usage(path):
            seen.append(str(path))
            if str(path) == source:
                raise PermissionError(path)
            return shutil._ntuple_diskusage(host_total, host_total - host_free, host_free)

        with mock.patch.object(incus, "query", pool.__getitem__), mock.patch.object(shutil, "disk_usage", disk_usage):
            return incus.capacity("p"), seen

    def test_loop_file_pool_is_held_to_the_fuller_host_disk(self):
        space, seen = self.capacity("/var/lib/incus/disks/p.img", host_free=50 * GiB)
        self.assertEqual(space, ((92 * GiB, 100 * GiB), (50 * GiB, 780 * GiB)))
        self.assertEqual(seen[-1], "/var/lib/incus/disks")
        problems = shortfalls(3, 2 * GiB, 64 * GiB, 128 * GiB, [], *space)
        self.assertEqual([problem.split(":")[0] for problem in problems], ["Disk"])

    def test_loop_file_with_less_room_than_the_host_limits_the_pool(self):
        space, _ = self.capacity("/var/lib/incus/disks/p.img", host_free=300 * GiB)
        self.assertEqual(shortfalls(18, 2 * GiB, 64 * GiB, 128 * GiB, [], *space), [])
        self.assertEqual(shortfalls(19, 2 * GiB, 64 * GiB, 128 * GiB, [], *space)[0].split(":")[0], "Disk")

    def test_block_device_pool_uses_its_own_numbers(self):
        space, seen = self.capacity("/dev/nvme1n1", host_free=1 * GiB)
        self.assertEqual((space, seen), (((92 * GiB, 100 * GiB), None), []))

    def test_host_reserve_does_not_shrink_a_loop_pool_that_is_the_tighter_limit(self):
        # 40 GiB left in the pool, 500 GiB free on a 725 GiB host: the 36 GiB host reserve is far from touched.
        space, _ = self.capacity("/var/lib/incus/disks/p.img", host_free=500 * GiB, used=60 * GiB, host_total=725 * GiB)
        self.assertEqual(shortfalls(1, 2 * GiB, 64 * GiB, 128 * GiB, [], *space), [])
        self.assertEqual(shortfalls(8, 2 * GiB, 64 * GiB, 128 * GiB, [], *space), [])
        self.assertEqual([problem.split(":")[0] for problem in shortfalls(9, 2 * GiB, 64 * GiB, 128 * GiB, [], *space)], ["Disk"])


class Sizes(unittest.TestCase):
    def test_incus_units(self):
        self.assertEqual((size("2GiB"), size("512MiB"), size("1GB"), size("1024")), (2 * GiB, GiB // 2, 10**9, 1024))

    def test_garbage_is_refused(self):
        with self.assertRaises(ValueError):
            size("lots")


class Reap(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.checkout = Path(self.directory.name)
        self.manifest = self.checkout / "cluster.json"
        self.manifest.write_text("{}")
        self.now = 1_000_000

    def tearDown(self):
        self.directory.cleanup()

    def run_record(self, pid=None, age=HOUR, **changes):
        record = dict(checkout=str(self.checkout), manifest=str(self.manifest), pid=pid or dead_pid())
        record.update(changes)
        return dict(owner=record, created=self.now - age)

    def test_young_run_of_an_exited_owner_is_kept(self):
        self.assertEqual(verdict(self.run_record(), self.now, DAY)[0], False)

    def test_old_run_of_an_exited_owner_is_removed(self):
        self.assertEqual(verdict(self.run_record(age=2 * DAY), self.now, DAY), (True, "older than 24h"))

    def test_gone_checkout_is_removed_at_any_age(self):
        self.assertEqual(verdict(self.run_record(checkout=str(self.checkout / "gone")), self.now, DAY), (True, "checkout gone"))

    def test_gone_manifest_is_removed_at_any_age(self):
        self.manifest.unlink()
        self.assertEqual(verdict(self.run_record(), self.now, DAY), (True, "manifest gone"))

    def test_live_owner_keeps_even_an_orphaned_old_run(self):
        run = self.run_record(pid=live_pid(), age=9 * DAY, checkout=str(self.checkout / "gone"))
        self.assertEqual(verdict(run, self.now, DAY)[0], False)
        self.assertEqual(verdict(run, self.now, DAY, force=True), (True, "checkout gone"))

    def test_reused_pid_is_not_the_owner(self):
        pid = os.getpid()
        self.assertTrue(owner_live(dict(pid=live_pid())))
        self.assertFalse(owner_live(dict(pid=f"{pid}:1")))

    def test_unrecorded_owner_goes_by_age_alone(self):
        self.assertEqual(verdict(dict(owner={}, created=self.now - 2 * DAY), self.now, DAY)[0], True)
        self.assertEqual(verdict(dict(owner={}, created=self.now - HOUR), self.now, DAY)[0], False)

    def test_unrecorded_run_without_vms_may_be_booting(self):
        self.assertEqual(verdict(dict(owner={}, created=None), self.now, DAY), (False, "age unknown"))


class Ownership(unittest.TestCase):
    def test_claude_session_and_process_are_recorded(self):
        record = owner("/checkout", "/checkout/core/.verify/x/cluster.json",
                       {"CLAUDE_PID": str(os.getpid()), "CLAUDE_CODE_SESSION_ID": "abc"})
        self.assertEqual(record["session"], "CLAUDE_CODE_SESSION_ID=abc")
        self.assertTrue(owner_live(record))

    def test_without_an_agent_the_calling_shell_owns_the_run(self):
        record = owner("/checkout", "/m", {})
        self.assertEqual((record["session"], record["pid"].split(":")[0]), ("", str(os.getppid())))


class Killer(unittest.TestCase):
    def test_earlyoom_is_named(self):
        log = "qemu-system-x86_64: terminating on signal 15 from pid 21679 (/usr/bin/earlyoom)\n"
        self.assertEqual(killer(log), "/usr/bin/earlyoom (signal 15)")

    def test_clean_log_names_nobody(self):
        self.assertIsNone(killer(""))


def live_pid():
    return owner("/", "/", {"CLAUDE_PID": str(os.getpid())})["pid"]


def dead_pid():
    child = subprocess.Popen(["true"])
    child.wait()
    return f"{child.pid}:123"


if __name__ == "__main__":
    unittest.main()
