"""Shared-host accounting for verify-cluster: who owns each run, whether it fits, what may be reaped."""

import os
from pathlib import Path
import re
import time

GiB = 1 << 30
UNITS = {"": 1, "B": 1, "KB": 10**3, "MB": 10**6, "GB": 10**9, "TB": 10**12,
         "KiB": 1 << 10, "MiB": 1 << 20, "GiB": 1 << 30, "TiB": 1 << 40}
# QEMU's own RSS on top of a guest that has touched all its memory; earlyoom logged
# 2017 MiB for 2 GiB guests, so the guest limit is the real ceiling.
VM_OVERHEAD = GiB // 4
# A fresh clone writes ~2.5 GiB of its sparse 24 GiB root; Machines that ran a
# feature check measured 3.5-4.7 GiB. Count the in-use figure, not the ceiling.
DISK_PER_MACHINE = 5 * GiB
# Exit status when a command failed because its Machine is not running.
INFRASTRUCTURE = 75
SESSION_VARIABLES = ["CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID", "T3_THREAD_ID"]


def size(text):
    """Bytes in an Incus size such as 2GiB or 512MB."""
    match = re.fullmatch(r"\s*(\d+(?:\.\d+)?)\s*([KMGT]i?B|B)?\s*", text)
    if not match:
        raise ValueError(f"Not a size: {text!r}")
    return int(float(match.group(1)) * UNITS[match.group(2) or ""])


def gib(count):
    return f"{count / GiB:.1f} GiB"


def process_start(pid):
    """The process's start time in clock ticks, which tells a live owner from a reused pid."""
    try:
        return int(Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19])
    except (OSError, IndexError, ValueError):
        return None


def owner(checkout, manifest, environment=os.environ):
    """Who started this run. CLAUDE_PID outlives one tool call; a human's shell is the parent."""
    pid = int(environment.get("CLAUDE_PID") or os.getppid())
    session = next((f"{name}={environment[name]}" for name in SESSION_VARIABLES if environment.get(name)), "")
    return dict(checkout=str(checkout), manifest=str(manifest), created=int(time.time()),
                session=session, pid=f"{pid}:{process_start(pid)}")


def owner_live(record):
    """True when the recorded owner process still runs, False when it exited, None when unrecorded."""
    if not record.get("pid"):
        return None
    pid, _, start = record["pid"].partition(":")
    if start == "None":
        return None
    return str(process_start(int(pid))) == start


def verdict(run, now, older_than, force=False):
    """Whether reap removes a run, and why. A plausibly live owner keeps its run unless forced."""
    record = run["owner"]
    if owner_live(record) and not force:
        return False, f"owner live (pid {record['pid'].partition(':')[0]})"
    if record.get("checkout") and not Path(record["checkout"]).is_dir():
        return True, "checkout gone"
    if record.get("manifest") and not Path(record["manifest"]).is_file():
        return True, "manifest gone"
    # A run from before owner records with no VMs yet may still be booting.
    if run["created"] is None:
        return False, "age unknown"
    hours = int(older_than // 3600)
    if now - run["created"] >= older_than:
        return True, f"older than {hours}h"
    return False, f"younger than {hours}h"


def committed(machines):
    """Memory the running guests may still claim: each limit minus what QEMU already holds."""
    return sum(max(machine["memory"] - machine["rss"], 0) for machine in machines if machine["status"] == "Running")


def shortfalls(machines, memory, available, total, running, pool, host_disk=None,
               memory_reserve=None, disk_reserve=None):
    """Why `machines` VMs of `memory` bytes do not fit, or an empty list.

    `available` is MemAvailable; `running` are the host's guests, whose untouched memory is
    already promised. `pool` and `host_disk` are (free, total) bytes; the disk reserve comes out
    of the host disk when the pool grows into one, else out of the pool. Reserves keep earlyoom
    (which acts at 5% free) and builds clear."""
    memory_reserve = total // 10 if memory_reserve is None else memory_reserve
    reserved_free, reserved_total = host_disk or pool
    disk_reserve = reserved_total // 20 if disk_reserve is None else disk_reserve
    problems = []
    usable = available - committed(running) - memory_reserve
    need = machines * (memory + VM_OVERHEAD)
    if need > usable:
        problems.append(f"RAM: need {gib(need)} for {machines} Machine(s), {gib(max(usable, 0))} usable "
                        f"({gib(available)} available, {gib(committed(running))} promised to running VMs, {gib(memory_reserve)} reserve)")
    need = machines * DISK_PER_MACHINE
    usable = min(pool[0], reserved_free - disk_reserve)
    if need > usable:
        host_free = f", {gib(host_disk[0])} free on the host disk" if host_disk else ""
        problems.append(f"Disk: need {gib(need)} for {machines} Machine(s), {gib(max(usable, 0))} usable "
                        f"({gib(pool[0])} free for VM roots{host_free}, {gib(disk_reserve)} reserve)")
    return problems


def killer(qemu_log):
    """What QEMU logged as its terminator, such as `/usr/bin/earlyoom (signal 15)`."""
    found = re.findall(r"terminating on signal (\d+) from pid \d+ \(([^)]*)\)", qemu_log)
    return f"{found[-1][1]} (signal {found[-1][0]})" if found else None
