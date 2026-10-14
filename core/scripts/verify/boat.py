"""Boat sandbox backend for verify-cluster's disposable-Machine contract."""

from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import fcntl
import hashlib
import host
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import sys
import threading
import time
import uuid

SNAPSHOT = "ployz-verify-024"
# The release ployzd install put in SNAPSHOT; Machines booting it skip the install.
SNAPSHOT_RELEASE = "0.2.4"
SIZE = "small"
# Launches per Machine before start() gives up on landing it on a node no peer uses.
PLACEMENTS = 3
MEMORY = 4 * host.GiB
TTL = int(os.environ.get("PLOYZ_VERIFY_BOAT_TTL", 3600))
# prepare-guest.sh pulls these into the Incus image; SNAPSHOT keeps no Docker images, so each Machine pulls them at boot.
IMAGES = ["caddy:2", "nginx:1.29-alpine", "nginx:1.27-alpine", "postgres:16-alpine", "alpine:3.20", "traefik/whoami:v1.10.3"]
UP_STATES = {"ready", "idle", "running", "cloning"}
# Registry states of a sandbox the run no longer holds.
GONE = {"deleted", "parked"}
REGISTRY = Path.home() / ".cache" / "ployz" / "verify" / "boat"
# prepare() bakes SNAPSHOT plus BAKE into a named snapshot whose authorized_keys holds this key, so Machines
# skip the image pulls and `boat exec`.
BOOTSTRAP = REGISTRY / "bootstrap"
BAKED = "ployz-verify-boat-"
# A Boat snapshot drops images no container uses, so BAKE leaves a created, never started holder per image.
# Boat restores containers after boot, so the holders stay; tmpfs over each image VOLUME keeps them volume-free.
# Boat keeps 10 named snapshots per account; bakes past the newest few are removed.
KEEP_BAKED = 3
NAMED = 10
BAKE_NAME = re.compile(re.escape(BAKED) + "[0-9a-f]{10}")
# Bakes this host made; another host or person may share the account and the name prefix.
BAKES = REGISTRY / "bakes.ledger"
BAKE = """
ufw disable >/dev/null
printf '%s\\n' "$@" | xargs -P8 -n1 docker pull -q >/dev/null
caddy_version=$(docker run --rm --entrypoint caddy caddy:2 version)
caddy_version=${caddy_version%% *}
docker tag caddy:2 "caddy:${caddy_version#v}"
for image in "$@" "caddy:${caddy_version#v}"; do
    mounts=$(docker image inspect -f '{{range $path, $_ := .Config.Volumes}}--mount=type=tmpfs,dst={{$path}} {{end}}' "$image")
    docker create --label ployz.verify.hold=1 $mounts "$image" >/dev/null
done
command -v zstd >/dev/null || DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zstd >/dev/null || \
    { apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zstd >/dev/null; }
sync
"""
# A clone can carry an sshd listener that systemd no longer tracks, so restarting ssh later fails with
# port 22 in use. Kill only listeners, so open SSH sessions survive.
LISTENER = shlex.quote(r"^sshd: [^ ]*sshd -D \[listener\]")
# A clone resumes while Boat's agent still queues systemd jobs, and restarting ssh under them can drop
# systemctl's bus connection. Two idle readings in a row mark the restore done.
SETTLE = ("idle=0; for i in $(seq 120); do if [ -z \"$(systemctl list-jobs --no-legend)\" ]; then idle=$((idle+1));"
          " [ $idle = 2 ] && break; else idle=0; fi; sleep 0.5; done; ")
# The ployz units live in /etc/systemd/system, where `systemctl mask` refuses them, so power off disables those.
PLOYZ_UNITS = "ployz.socket ployz.service ployz-volume-plugin.socket"
DOCKER_UNITS = "docker.service docker.socket containerd.service"
# `down` parks a sandbox it proved clean here, and the next `up` of the same bake claims it instead of spending a start.
POOL = REGISTRY / "pool.ledger"
# Pool entries being deleted. Checkouts before it existed read only POOL, so none of them claims a sandbox this one retires.
RETIRING = REGISTRY / "retiring.ledger"
POOL_SIZE = int(os.environ.get("PLOYZ_VERIFY_BOAT_POOL", 6))
# The bake's own ployz install, relative to /; a run's install overwrites it, so reset restores it from STASH.
INSTALLED = ("usr/local/bin/ployz* etc/systemd/system/ployz* etc/systemd/system/*.wants/ployz* etc/docker/daemon.json "
             "etc/modprobe.d/ployz* etc/modules-load.d/ployz* etc/default/ployz*")
# Files a run replaces outside INSTALLED: test-verify-warm-move.py swaps runc for a wrapper and keeps runc.<tag>-original.
REPLACED = "usr/bin/runc"
STASH = "/var/lib/ployz-verify-bake.tar"


def scratch(depth=""):
    """find over the directories a run can litter that the base image fills itself, relative to /, before its action.

    Boat's agent keeps live state in var/tmp/ariana, and systemd names new private directories every boot, so both are skipped."""
    return f"find root srv var/tmp -mindepth 1 {depth}\\( -path 'var/tmp/systemd-private-*' -o -path var/tmp/ariana \\) -prune -o"


# Linux's default ip_local_port_range: avahi, Boat's wg0 and the kernel pick a new port here on every boot.
EPHEMERAL = (32768, 60999)
# Each repo:tag and image id a fresh clone holds once Docker restored the bake's containers; reset brings the tags back
# and removes every other image.
TAGS = STASH + ".tags"
IMAGE_TAGS = "docker image ls --no-trunc --format '{{.Repository}}:{{.Tag}} {{.ID}}' | LC_ALL=C sort -u"
# Boat restores the bake's containers lazily after boot; until every holder is back, Docker's state is still moving.
HOLDS = (f"for i in $(seq 240); do [ \"$(docker ps -aq --filter label=ployz.verify.hold=1 | wc -l)\" -ge {len(IMAGES) + 2} ]"
         " && break; sleep 0.5; done; ")
RECORD_IMAGES = HOLDS + f"[ -e {TAGS} ] || {{ " + IMAGE_TAGS + f" > {TAGS}.next && mv {TAGS}.next {TAGS}; }}\n"
# Puts back each tag a run moved, untags what the bake lacks, removes every image the bake did not hold, and empties
# the build cache, which a fresh clone holds none of.
IMAGE_RESET = r"""
[ -s "$stash.tags" ] || { echo "no record of the bake's images" >&2; exit 1; }
while read -r tag id; do case $tag in *"<none>"*) ;; *) docker tag "$id" "$tag" ;; esac; done < "$stash.tags"
for tag in $(""" + IMAGE_TAGS + r""" | LC_ALL=C comm -13 "$stash.tags" - | awk '$1 !~ /<none>/ {print $1}'); do
    docker rmi -f "$tag" >/dev/null
done
extra=$(docker image ls -q --no-trunc | LC_ALL=C sort -u | LC_ALL=C comm -23 - <(awk '{print $2}' "$stash.tags" | LC_ALL=C sort -u))
if [ -n "$extra" ]; then docker rmi -f $extra >/dev/null; fi
docker builder prune -af >/dev/null
"""
KEEP_INSTALL = f"""
if [ ! -e {STASH} ]; then
    cd / && shopt -s nullglob && found=()
    # Reset deletes every scratch path this listing lacks.
    {scratch()} -print0 2>/dev/null | LC_ALL=C sort -z > {STASH}.scratch
    for path in {INSTALLED} {REPLACED} var/lib/ployz; do if [ -e "$path" ] || [ -L "$path" ]; then found+=("$path"); fi; done
    tar -cpf {STASH}.next "${{found[@]}}" && sync {STASH}.next && mv {STASH}.next {STASH}
fi
"""
# What a run can leave behind, as a fresh clone of the bake shows it; reset must bring every section back.
FINGERPRINT = SETTLE + HOLDS + r"""
section() { printf '== %s\n' "$1"; }
section containers; docker ps -a --format '{{.Names}}|{{.Image}}|{{.State}}|{{.Label "ployz.verify.hold"}}' 2>&1
section images; """ + IMAGE_TAGS + r""" 2>&1
section build-cache; docker system df --format '{{.Type}} {{.TotalCount}}' 2>&1 | grep -i build
section volumes; docker volume ls --format '{{.Driver}} {{.Name}}' 2>&1
section networks; docker network ls --format '{{.Name}} {{.Driver}}' 2>&1
section zpools; if command -v zpool >/dev/null; then zpool list -H -o name 2>&1; fi
section links; ip -br link | awk '{print $1}'
section paths; find /var/lib /run /opt -mindepth 1 -maxdepth 1 -name '*ployz*' -printf '%p %y\n' 2>/dev/null
find /var/lib/ployz /run/ployz /var/lib/ployz-volumes /var/lib/ployz-mirror /var/lib/ployz-machine-pool \
    /opt/ployz-verify /run/ployz-containerd -mindepth 1 -printf '%p %y\n' 2>/dev/null
section units; find /etc/systemd/system /run/systemd/system -name 'ployz*' -printf '%p %y %l\n' 2>/dev/null
section contents; find /usr/local/bin /etc/systemd/system /etc/docker /etc/modprobe.d /etc/modules-load.d -maxdepth 1 \
    -type f \( -name 'ployz*' -o -name daemon.json \) -exec sha256sum {} + 2>/dev/null
find /var/lib/ployz -type f -exec sha256sum {} + 2>/dev/null
section unit-files; systemctl list-unit-files --no-legend 'ployz*'
section enabled; systemctl list-unit-files --no-legend --state=enabled | awk '{print $1}'
section listening; ss -Hlntup | awk '{print $1, $5, $7}'
section authorized; sha256sum /root/.ssh/authorized_keys
section replaced; sha256sum /usr/bin/runc 2>&1; find /usr/bin -maxdepth 1 -name 'runc.*' 2>/dev/null
section scratch; cd / && """ + scratch("-maxdepth 2 ") + r""" -printf '%p %y\n' 2>/dev/null
true
"""
# Puts back the bake's files from "$stash", relative to the working directory, which reset makes /.
RESTORE = r"""
shopt -s nullglob
rm -rf """ + INSTALLED + " " + REPLACED + " " + REPLACED + r""".* run/systemd/system/ployz*
# /var/lib/ployz can be a mount point, so it stays and only its contents go.
find var/lib/ployz -mindepth 1 -delete
find var/lib run opt -mindepth 1 -maxdepth 1 -name '*ployz*' ! -path var/lib/ployz ! -path "${stash#/}*" -exec rm -rf {} +
[ -e "$stash.scratch" ] || { echo "no record of the bake's scratch paths" >&2; exit 1; }
""" + scratch() + r""" -print0 2>/dev/null | LC_ALL=C sort -z | LC_ALL=C comm -z -13 "$stash.scratch" - | xargs -0r rm -rf --
tar -xpf "$stash" -C .
"""
# Drops the run's logs, relative to the working directory, which reset makes /: journald closes its files so
# they become archives, and the archives and every other machine's journal go. The next boot logs to a fresh file.
JOURNAL_RESET = r"""
journalctl --rotate
if [ -d var/log/journal ]; then
    machine=$(cat etc/machine-id)
    find var/log/journal -mindepth 1 -maxdepth 1 ! -name "$machine" -exec rm -rf {} +
    find "var/log/journal/$machine" -mindepth 1 -name '*@*' -delete
fi
"""
# Returns the guest to the bake: what uninstall.sh removes, everything else a run makes, then the bake's install.
RESET = r"""
stash=$1 key=$2
tar -tf "$stash" """ + REPLACED + r""" >/dev/null || { echo "the bake's install keeps no runc" >&2; exit 1; }
systemctl unmask docker.service docker.socket containerd.service >/dev/null
units=$(systemctl list-units --all --plain --no-legend 'ployz*' | awk '{print $1}')
if [ -n "$units" ]; then systemctl stop $units; fi
systemctl start docker
others=$(docker ps -aq --filter label=ployz.verify.hold=1 | sort | comm -13 - <(docker ps -aq | sort))
if [ -n "$others" ]; then docker rm -f $others >/dev/null; fi
volumes=$(docker volume ls -q)
if [ -n "$volumes" ]; then docker volume rm -f $volumes >/dev/null; fi
networks=$(docker network ls --format '{{.Name}}' | grep -vxE 'bridge|host|none' || true)
if [ -n "$networks" ]; then docker network rm $networks >/dev/null; fi
""" + IMAGE_RESET + r"""
observe="$(docker info -f '{{.DockerRootDir}}')/ployz-observe"
if command -v zpool >/dev/null; then for pool in $(zpool list -H -o name); do zpool destroy -f "$pool"; done; fi
# Boat's own WireGuard link is wg0; only ployz's links go.
for link in $(ip -o link show | awk -F': ' '{print $2}' | cut -d@ -f1 | grep '^ployz' || true); do ip link del "$link"; done
rm -rf "$observe"
cd /
""" + RESTORE + r"""
install -d -m 700 /root/.ssh
printf '%s\n' "$key" > /root/.ssh/authorized_keys
chmod 600 /root/.ssh/authorized_keys
""" + JOURNAL_RESET + r"""
sync
"""


def cli():
    return [os.environ.get("BOAT_CLI", "boat"), "--no-update"]


def each(function, items):
    if not items:
        return []
    with ThreadPoolExecutor(max_workers=len(items)) as workers:
        return list(workers.map(function, items))


def error(result):
    """The `{"event":"error"}` line a failed Boat command ends with, or None."""
    for line in reversed(result.stdout.decode(errors="replace").splitlines()):
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if isinstance(event, dict) and event.get("event") == "error":
            return event
    return None


def exited(result):
    """The stderr of a command `boat exec` ran that exited non-zero, or None."""
    try:
        ran = json.loads(result.stdout)
    except ValueError:
        return None
    if isinstance(ran, dict) and ran.get("exitCode"):
        return f"exit {ran['exitCode']}: {ran.get('stderr', '').strip()}"
    return None


def events(result):
    found = []
    for line in result.stdout.decode(errors="replace").splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if isinstance(event, dict):
            found.append(event)
    return found


def endpoint(text):
    """Host and port from a sandbox's sshEndpoint, such as 213.32.24.158:19040."""
    address, _, port = text.rpartition(":")
    return address.strip("[]"), int(port)


def boat_node(address):
    """The Boat node a sandbox runs on: each node hands out one IPv6 /80."""
    return str(ipaddress.IPv6Network(f"{address}/80", strict=False))


def registry(run_id):
    return REGISTRY / f"{run_id}.json"


def read(path):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return None


def records():
    """Every run record in REGISTRY; a corrupt one is skipped with a warning so it never hides the others."""
    for path in sorted(REGISTRY.glob("*.json")):
        try:
            record = read(path)
        except ValueError:
            print(f"Skipping unreadable Boat run record {path}", file=sys.stderr)
            continue
        if record:
            yield record


def write(path, record):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(f".{os.getpid()}.{threading.get_ident()}.tmp")
    temporary.write_text(json.dumps(record, indent=2) + "\n")
    temporary.replace(path)


COMPRESSED = {}
COMPRESSING = threading.Lock()


def compressed(source):
    """zstd of `source`, made once per content for every Machine that receives it."""
    stat = os.stat(source)
    key = (os.path.realpath(source), stat.st_size, stat.st_mtime_ns)
    with COMPRESSING:
        if key not in COMPRESSED:
            COMPRESSED[key] = subprocess.run(["zstd", "-3", "-T0", "-qc", source], capture_output=True, check=True).stdout
        return COMPRESSED[key]


def bootstrap():
    """The public half of the key every baked snapshot authorizes, made on first use."""
    BOOTSTRAP.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (BOOTSTRAP / "key.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if not (BOOTSTRAP / "id_ed25519").exists():
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "ployz-verify-boat",
                            "-f", BOOTSTRAP / "id_ed25519"], check=True)
    return (BOOTSTRAP / "id_ed25519.pub").read_text().strip()


def stale_bakes(named, current, ours, in_use):
    """Bakes to remove before baking `current`, oldest first: only bakes this host made that no run still uses."""
    older = sorted((snapshot for snapshot in named if snapshot["name"] in ours and BAKE_NAME.fullmatch(snapshot["name"])
                    and snapshot["name"] != current and snapshot["name"] not in in_use),
                   key=lambda snapshot: snapshot["createdAt"])
    # The account keeps at most NAMED snapshots, so a full account gives up more of our older bakes.
    # A failed or unfinished `current` is saved over, so it takes no extra slot.
    taken = sum(snapshot["name"] != current for snapshot in named)
    surplus = max(len(older) - (KEEP_BAKED - 1), taken - (NAMED - 1), 0)
    return [snapshot["name"] for snapshot in older[:surplus]]


def images_in_use():
    return ({record["image"] for record in records() if record.get("image")}
            | {entry["image"] for entry in read(POOL) or []})


def registered():
    """Ids some run's registry holds and has neither deleted nor parked."""
    return {entry["id"] for record in records() for entry in record.get("sandboxes", {}).values()
            if entry.get("id") and entry.get("state") not in GONE}


@contextmanager
def locked(name):
    """REGISTRY/name, locked against every other run and thread on this host."""
    REGISTRY.mkdir(parents=True, exist_ok=True)
    with (REGISTRY / name).open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


@contextmanager
def pool():
    """The parked and retiring sandboxes, locked against every other run on this host; changes to the list are saved on exit."""
    with locked("pool.lock"):
        retiring = [dict(entry, retiring=True) for entry in read(RETIRING) or []]
        doomed = {entry["id"] for entry in retiring}
        entries = [entry for entry in read(POOL) or [] if entry["id"] not in doomed] + retiring
        yield entries
        # RETIRING first: a crash between the writes leaves an entry in both, and retiring wins.
        write(RETIRING, [entry for entry in entries if entry.get("retiring")])
        write(POOL, [entry for entry in entries if not entry.get("retiring")])


def pick(entries, image, want):
    """Parked sandboxes of `image` a run may claim, newest first and one per node; tidy() the entries first."""
    picked, nodes = [], set()
    for entry in sorted(entries, key=lambda entry: -entry["parked_at"]):
        if entry["image"] == image and not entry.get("retiring") and len(picked) < want and boat_node(entry["ip"]) not in nodes:
            picked.append(entry)
            nodes.add(boat_node(entry["ip"]))
    return picked


def sandbox_states():
    """The state of every sandbox this Boat key can see, by id."""
    return {sandbox["id"]: sandbox["state"] for sandbox in boat_json("list", "--all")["sandboxes"]}


def kept_bakes():
    """Bakes this host still keeps or a run still uses; a parked sandbox of any other bake is retired.

    Another checkout can run a newer bake than this one, so no single checkout's bake decides which bakes are old."""
    return set(read(BAKES) or []) | {record["image"] for record in records() if record.get("image")}


def stale(entries, states, kept, force=False):
    """Parked sandboxes to remove, with why: gone, stopped, of a retired bake, or every one under --force."""
    found = []
    for entry in entries:
        state = states.get(entry["id"])
        if state is None:
            found.append((entry, "gone"))
        elif state not in UP_STATES:
            found.append((entry, state))
        elif entry.get("retiring"):
            found.append((entry, "retiring"))
        elif entry["image"] not in kept:
            found.append((entry, "retired bake"))
        elif force:
            found.append((entry, "--force"))
    return found


def tidy(entries, states, owned):
    """Drop entries some run holds and mark stale() ones retiring; returns those for the caller to retire() off the lock."""
    entries[:] = [entry for entry in entries if entry["id"] not in owned]
    dead = [entry for entry, reason in stale(entries, states, kept_bakes())]
    for entry in dead:
        entry["retiring"] = True
    return dead


def deleted(result):
    """Whether a `boat delete` left the sandbox gone."""
    return not result.returncode or (error(result) or {}).get("status") == 404


def retire(dead, delete):
    """Delete retiring pool entries, then drop from the pool each one Boat confirms gone; returns each delete's result.

    An entry stays in the pool until then, so a failed delete or a crash leaves it for the next tidy() rather than orphaned."""
    results = each(lambda entry: delete(entry["id"]), dead)
    gone = {entry["id"] for entry, result in zip(dead, results) if deleted(result)}
    if gone:
        with pool() as entries:
            entries[:] = [entry for entry in entries if entry["id"] not in gone]
    return results


def forget_fingerprints(image):
    """Remove every fingerprint of `image` that fingerprints() named under any key."""
    saved = re.compile(re.escape(image) + r"(\.[0-9a-f]{10})?\.fingerprint")
    for path in REGISTRY.glob(f"{image}.*fingerprint"):
        if saved.fullmatch(path.name):
            path.unlink(missing_ok=True)


def fingerprint(text):
    """FINGERPRINT's output by section, without hold container names, ephemeral ports, or pids."""
    fields, current = {}, None
    for line in text.splitlines():
        if line.startswith("== "):
            current = fields.setdefault(line[3:], [])
        elif current is not None and line.strip():
            if line.count("|") == 3 and line.endswith("|1"):
                line = "hold|" + line.split("|", 1)[1]
            elif current is fields.get("listening"):
                line = listener(line)
            current.append(line)
    return {name: sorted(lines) for name, lines in fields.items()}


def listener(line):
    """`proto address:port users:(...)` without pids, ephemeral ports or lease addresses, so clones compare equal."""
    proto, address, *users = line.split(maxsplit=2)
    host, _, port = address.rpartition(":")
    if "%" in host:
        # An interface-bound listener, such as the DHCP client, holds that sandbox's own lease address.
        host = "%" + host.split("%")[1]
    if port.isdigit() and EPHEMERAL[0] <= int(port) <= EPHEMERAL[1]:
        port = "ephemeral"
    names = sorted(set(re.findall(r'\("([^"]+)"', users[0]))) if users else []
    return " ".join([proto, f"{host}:{port}", *names])


def differences(expected, actual):
    """Each section that differs, with the lines only one side has."""
    found = []
    for name in sorted(expected.keys() | actual.keys()):
        if expected.get(name) != actual.get(name):
            lost = sorted((Counter(expected.get(name, [])) - Counter(actual.get(name, []))).elements())
            gained = sorted((Counter(actual.get(name, [])) - Counter(expected.get(name, []))).elements())
            found.append(f"{name} (-{lost[:3]} +{gained[:3]})")
    return found


# Bump when fingerprint() or listener() read FINGERPRINT's output differently, so no saved fingerprint outlives its reading.
NORMALIZE = 1


def fingerprints(image):
    """Where the first fresh clone's fingerprint of `image` lives; a new FINGERPRINT script or reading starts a new one."""
    script = hashlib.sha256(f"{NORMALIZE}\n{FINGERPRINT}".encode()).hexdigest()[:10]
    return REGISTRY / f"{image}.{script}.fingerprint"


def corrosion_image(core):
    return re.search(r'pub const IMAGE: &str = "([^"]+)"', (core / "crates/ployzd/src/corrosion/service.rs").read_text()).group(1)


def bake_name(corrosion):
    recipe = json.dumps([SNAPSHOT, BAKE, SETTLE, IMAGES, corrosion, bootstrap()])
    return BAKED + hashlib.sha256(recipe.encode()).hexdigest()[:10]


def pooled():
    """How many sandboxes are parked, per bake."""
    counts = {}
    for entry in read(POOL) or []:
        counts[entry["image"]] = counts.get(entry["image"], 0) + 1
    return counts


def reap_pool(force, yes):
    """Pool rows for reap: sandboxes stale() names; with yes, delete them and drop them from the pool."""
    with pool() as entries:
        # States read under the lock cannot miss a sandbox a run parks or claims meanwhile. A run can hold a retiring
        # entry too: a crash between pool()'s two writes leaves it in pool.ledger, where main's claim takes it.
        owned = registered()
        entries[:] = [entry for entry in entries if entry["id"] not in owned]
        found = stale(entries, sandbox_states(), kept_bakes(), force)
        if yes:
            for entry, reason in found:
                entry["retiring"] = True
    rows = [dict(id=entry["id"], reason=reason, action="remove") for entry, reason in found]
    if not yes:
        return rows
    results = retire([entry for entry, reason in found], lambda sandbox: subprocess.run(
        [*cli(), "delete", sandbox, "--yes", "--json"], stdin=subprocess.DEVNULL, capture_output=True, timeout=60))
    for row, result in zip(rows, results):
        if deleted(result):
            row["action"] = "removed"
        else:
            row.update(action="failed", reason=str(error(result) or result.stderr.decode(errors="replace").strip()).splitlines()[0])
    return rows


def boat_json(*args):
    result = subprocess.run([*cli(), *args, "--json"], stdin=subprocess.DEVNULL, capture_output=True, timeout=60)
    if result.returncode:
        found = error(result) or {}
        raise RuntimeError(f"boat {args[0]} failed: {found.get('code') or result.stderr.decode(errors='replace').strip()}")
    return json.loads(result.stdout)


def status(state):
    return "Running" if state in UP_STATES else (state or "gone").capitalize()


def runs():
    """Every Boat verify run this user's registry holds, from any checkout."""
    found = list(records())
    if not found:
        return []
    states = sandbox_states()
    return [dict(id=record["run"], provider="boat", owner=record.get("owner", {}), created=record.get("created"),
                 machines=sorted((dict(name=name, sandbox=entry["id"], status=status(states.get(entry["id"])), rss=0,
                                       memory=MEMORY, created=entry.get("created", record.get("created")))
                                  for name, entry in record["sandboxes"].items() if entry["state"] not in GONE),
                                 key=lambda machine: machine["name"]))
            for record in found]


def shortfalls(machines, limits):
    """Why Boat cannot start `machines` sandboxes now, or an empty list."""
    problems = []
    headroom = limits["maxActiveSandboxes"] - limits["activeSandboxes"]
    if machines > headroom:
        problems.append(f"Active sandboxes: need {machines}, {headroom} of {limits['maxActiveSandboxes']} free")
    if limits["starts"].get("unlimited"):
        return problems
    # Relaunches wait on a boot, so only the first placements share a minute; the rate_limited retry absorbs the rest.
    for window, need in [("minute", machines), ("hour", machines * PLACEMENTS), ("day", machines * PLACEMENTS)]:
        remaining = limits["starts"][window]["remaining"]
        if need > remaining:
            problems.append(f"Starts this {window}: need up to {need}, {remaining} left")
    return problems


def preflight(machines, core):
    fresh = machines
    if POOL_SIZE:
        entries = read(POOL) or []
        tidy(entries, sandbox_states(), registered())
        fresh -= len(pick(entries, bake_name(corrosion_image(core)), machines))
    problems = shortfalls(fresh, boat_json("limits"))
    if problems:
        raise RuntimeError("Boat cannot start the run now.\n" + "\n".join(problems))


class Provider:
    def __init__(self, run, commands, core):
        self.run, self.commands, self.core = run, commands, core
        self.registry = registry(run["id"])
        self.private = Path(run["manifest"]).parent / "private"
        # A socket path must fit in 108 bytes with ssh's 17-character bind suffix; neither the run nor the registry directory does.
        # The per-user runtime directory is private when it is ours; /tmp is the fallback options() checks.
        runtime = os.environ.get("XDG_RUNTIME_DIR")
        try:
            ours = bool(runtime) and os.stat(runtime).st_uid == os.getuid() and Path(runtime).is_dir()
        except OSError:
            ours = False
        private = Path(runtime or "/") / "ployz-boat"
        # "/<8>-<%C as 40>" plus the bind suffix is 67 more characters.
        self.control = private if ours and len(os.fsencode(private)) + 67 <= 107 else Path(f"/tmp/ployz-boat-{os.getuid()}")

    def boat(self, *args, **kwargs):
        return self.commands([*cli(), *args, "--json"], **kwargs)

    def record(self, update=None, missing_ok=False):
        """The run's registry; `update` changes it under a host-wide lock, since another process can park or remove it.

        An update to a registry `down` already removed raises, unless `missing_ok` makes it a no-op."""
        with locked("registry.lock"):
            record = read(self.registry)
            if update and record is None and not missing_ok:
                raise RuntimeError(f"Boat registry for this run is gone: {self.registry}")
            if update and record is not None:
                update(record)
                write(self.registry, record)
            return record

    def sandbox(self, node):
        return self.record()["sandboxes"][node["name"]]

    def mark(self, name, **fields):
        self.record(lambda record: record["sandboxes"].setdefault(name, {}).update(fields))

    def create(self):
        if self.registry.exists():
            raise RuntimeError(f"Boat registry already exists for this run: {self.registry}")
        write(self.registry, dict(run=self.run["id"], manifest=self.run["manifest"], owner=self.run.get("owner", {}),
                                  image=self.run.get("image"), created=int(time.time()), sandboxes={},
                                  # Each sandbox is claimed or launched with a full TTL after this, so it bounds the last extend.
                                  extended=int(time.time())))
        if POOL_SIZE and self.run.get("image") not in (None, SNAPSHOT):
            self.claim()

    def claim(self):
        """Give Machines parked sandboxes of this bake, each proven clean again; start() launches the rest."""
        names = [node["name"] for node in self.run.get("machines", [])]
        with pool() as entries:
            # A park marks its run's entry parked under this lock, so ownership and states read here cannot go stale.
            dead = tidy(entries, sandbox_states(), registered())
            picked = pick(entries, self.run["image"], len(names))
            entries[:] = [entry for entry in entries if entry not in picked]
            # The registry takes them before the pool lets go, so no other run or reap can.
            self.record(lambda record: record["sandboxes"].update(
                {name: dict(id=entry["id"], state="running", created=int(time.time()), ip=entry["ip"], ssh=entry["ssh"],
                            reused=True) for name, entry in zip(names, picked)}))
        retire(dead, lambda sandbox: self.boat("delete", sandbox, "--yes", check=False))
        expected = read(fingerprints(self.run["image"]))

        def verify(name):
            node, sandbox = dict(name=name), self.sandbox(dict(name=name))["id"]
            extended = self.boat("extend", sandbox, "--ttl", TTL, check=False)
            if extended.returncode:
                return self.discard(name, f"extend failed: {error(extended) or extended.stderr.decode(errors='replace').strip()}")
            found = differences(expected or {}, self.fingerprint(node)) if expected else ["no fingerprint of this bake"]
            if found:
                return self.discard(name, "differs from a fresh clone in " + "; ".join(found))
        each(verify, names[:len(picked)])
        # preflight counted every candidate as covered; a discarded one needs a start preflight never budgeted.
        reused = sum(bool(entry.get("reused")) for name, entry in self.record()["sandboxes"].items() if name in names)
        problems = shortfalls(len(names) - reused, boat_json("limits")) if reused < len(names) else []
        if problems:
            raise RuntimeError("Boat cannot start the Machines the pool did not cover now.\n" + "\n".join(problems))

    def discard(self, name, why):
        """Delete a claimed sandbox that is not provably clean, so start() launches a fresh one in its place."""
        sandbox = self.sandbox(dict(name=name))["id"]
        print(f"{name}: not reusing {sandbox}: {why}", file=sys.stderr)
        gone = deleted(self.boat("delete", sandbox, "--yes", check=False))

        def drop(record):
            entry = record["sandboxes"].pop(name)
            if not gone:
                # down deletes it; it never runs as a Machine.
                record["sandboxes"][f"{name}-discarded"] = {**entry, "reused": False, "discarded": True}
        self.record(drop)

    def fingerprint(self, node):
        result = self.exec(node, ["bash", "-c", FINGERPRINT], check=False, timeout=120)
        if result.returncode:
            return {"ssh": [f"exit {result.returncode}: {result.stderr.decode(errors='replace').strip()[-200:]}"]}
        return fingerprint(result.stdout.decode(errors="replace"))

    def launch(self, node):
        before = time.time()
        for attempt in range(10):
            result = self.boat("new", "--from", self.run["image"], "--type", SIZE, "--ttl", TTL, check=False)
            found = events(result)
            created = next((event["id"] for event in found if event.get("event") == "created"), None)
            if created:
                self.mark(node["name"], id=created, state="running", created=int(before))
            failure = error(result)
            if not result.returncode and created:
                return created
            if failure and failure.get("code") == "boat_starting" and not created:
                time.sleep(min(2 ** attempt, 10))
                continue
            if failure and failure.get("code") == "rate_limited" and not created:
                # Other runs can spend this minute's starts after our preflight read them.
                time.sleep(15)
                continue
            if failure and failure.get("code") == "boat_direct_failed" and not created:
                # The create may have run without telling us its id; never retry it blind.
                known = ({entry["id"] for record in records() for entry in record.get("sandboxes", {}).values()}
                         | {entry["id"] for entry in (read(POOL) or []) + (read(RETIRING) or [])})
                unknown = [sandbox["id"] for sandbox in boat_json("list", "--all")["sandboxes"]
                           if sandbox["id"] not in known and sandbox["createdAt"] >= time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(before - 5))]
                raise RuntimeError(f"{node['name']}: boat new failed with boat_direct_failed; sandboxes created since then "
                                   f"that no run owns: {', '.join(unknown) or 'none'}. Delete any that are yours.")
            raise RuntimeError(f"{node['name']}: boat new failed: {failure or result.stderr.decode(errors='replace')}")
        raise RuntimeError(f"{node['name']}: boat new kept answering boat_starting or rate_limited")

    def place(self, name, info):
        """Record a sandbox's address unless a peer in this run already holds its node; True when recorded."""
        placed = []

        def claim(record):
            taken = {boat_node(entry["ip"]) for other, entry in record["sandboxes"].items()
                     if other != name and entry.get("ip") and entry.get("state") != "deleted"}
            if boat_node(info["ip"]) not in taken:
                record["sandboxes"][name].update(ssh=info["sshEndpoint"], ip=info["ip"])
                placed.append(True)
        self.record(claim)
        return bool(placed)

    def start(self, node):
        reused = self.record()["sandboxes"].get(node["name"], {}).get("reused")
        for attempt in range(0 if reused else PLACEMENTS):
            sandbox = self.launch(node)
            info = json.loads(self.boat("info", sandbox).stdout)["sandbox"]
            if self.place(node["name"], info):
                break
            # Peers on one node share an on-link /80 and their WireGuard over IPv6 never handshakes.
            self.boat("delete", sandbox, "--yes")
        else:
            if not reused:
                raise RuntimeError(f"{node['name']}: Boat kept placing the sandbox on a node another Machine uses")
        if self.run["image"] == SNAPSHOT:
            self.install_key(node, sandbox)
        self.wait_agent(node)
        # A baked sandbox already holds BAKE's result; it keeps its own ployz install for reset to restore.
        bake = BAKE if self.run["image"] == SNAPSHOT else KEEP_INSTALL + (RECORD_IMAGES if POOL_SIZE else "")
        self.exec(node, ["bash", "-ec", f"""
{SETTLE}
main=$(systemctl show -p MainPID --value ssh.service)
for pid in $(pgrep -f {LISTENER}); do [ "$pid" = "$main" ] || kill "$pid"; done
if [ -n "$(ss -Hlun 'sport = :51820')" ]; then echo "UDP port 51820 is taken" >&2; exit 1; fi
{bake}""", "boot", self.run["corrosion"], *IMAGES], timeout=600)
        if POOL_SIZE and not reused and self.run["image"] != SNAPSHOT and not fingerprints(self.run["image"]).exists():
            # The first fresh clone of a bake records what reset must return every later sandbox to.
            captured = self.fingerprint(node)
            if "ssh" not in captured:
                write(fingerprints(self.run["image"]), captured)

    def install_key(self, node, sandbox):
        key = (self.private / "id_ed25519.pub").read_text().strip()
        install = (SETTLE + f"install -d -m 700 /root/.ssh && printf '%s\\n' {shlex.quote(key)} > /root/.ssh/authorized_keys"
                   " && chmod 600 /root/.ssh/authorized_keys && systemctl stop ssh.service ssh.socket"
                   f" && {{ pkill -f {LISTENER}; for i in $(seq 50); do pgrep -f {LISTENER} >/dev/null || break; sleep 0.1; done; }}"
                   " && systemctl start ssh.socket")
        deadline = time.monotonic() + 120
        while True:
            result = self.boat("exec", sandbox, "--timeout", "60", "--", "sudo", "sh", "-c", install, check=False)
            if not result.returncode:
                return
            failure = error(result)
            if (failure or {}).get("code") != "boat_starting" or time.monotonic() >= deadline:
                why = failure or exited(result) or result.stderr.decode(errors="replace")
                raise RuntimeError(f"{node['name']}: could not install the root key: {why}")
            time.sleep(1)

    def power_off(self, node):
        # No sync beyond the unit links: a reset loses the page cache, as `incus stop --force` does.
        # `boat stop` is not used because a resumed sandbox can land on another host with a new address.
        self.exec(node, ["bash", "-ec", f"""
systemctl mask {DOCKER_UNITS} >/dev/null
observe=
if [ -e /etc/systemd/system/ployz-observe.service ]; then observe=ployz-observe.service; fi
systemctl disable {PLOYZ_UNITS} $observe >/dev/null 2>&1
sync /etc/systemd/system /etc/systemd/system/*.wants
setsid sh -c 'sleep 1; echo b > /proc/sysrq-trigger' >/dev/null 2>&1 < /dev/null &
"""])
        time.sleep(5)
        host, port = endpoint(self.sandbox(node)["ssh"])
        self.commands(["ssh", *self.options(), "-p", port, "-O", "exit", "root@" + host], check=False)

    def power_on(self, node):
        self.wait_agent(node)
        self.exec(node, ["bash", "-ec", f"""
systemctl unmask {DOCKER_UNITS} >/dev/null
systemctl start docker
observe=
if [ -e /etc/systemd/system/ployz-observe.service ]; then observe=ployz-observe.service; fi
systemctl enable --now {PLOYZ_UNITS} $observe
"""])

    def state(self, node):
        if node["name"] in self.run.get("powered_off", []):
            return "STOPPED"
        sandbox = self.record()["sandboxes"].get(node["name"], {})
        if not sandbox.get("id") or sandbox.get("state") == "deleted":
            return ""
        result = self.boat("info", sandbox["id"], check=False)
        if result.returncode:
            return "" if (error(result) or {}).get("status") == 404 else "UNKNOWN"
        state = json.loads(result.stdout)["sandbox"]["state"]
        return "RUNNING" if state in UP_STATES else state.upper()

    def stop_cause(self, node):
        return None

    def refresh(self):
        """Push each live sandbox's auto-stop TTL seconds out once half the TTL is spent, so a long suite keeps its Machines."""
        record, now = self.record(), time.time()
        if not record or now - record.get("extended", 0) < TTL / 2:
            return
        ids = [entry["id"] for entry in record.get("sandboxes", {}).values()
               if entry.get("id") and entry.get("state") == "running"]
        if not ids:
            return
        results = each(lambda sandbox: self.boat("extend", sandbox, "--ttl", TTL, check=False), ids)
        failed = [sandbox for sandbox, result in zip(ids, results) if result.returncode]
        if failed:
            print(f"Could not extend the auto-stop of {', '.join(failed)}; they stop on their earlier deadline.", file=sys.stderr)
        else:
            self.record(lambda record: record.update(extended=int(now)), missing_ok=True)

    def wait_agent(self, node):
        deadline = time.monotonic() + 180
        while True:
            if not self.exec(node, ["true"], check=False, timeout=10).returncode:
                return
            if time.monotonic() >= deadline:
                raise RuntimeError(f"{node['name']}: root SSH to the sandbox did not answer")
            time.sleep(1)

    def options(self):
        self.control.mkdir(parents=True, exist_ok=True, mode=0o700)
        # /tmp is shared; a directory someone else planted could hand our ssh to their fake control master.
        found = os.lstat(self.control)
        if not stat.S_ISDIR(found.st_mode) or found.st_uid != os.getuid() or found.st_mode & 0o077:
            raise RuntimeError(f"{self.control} must be a directory only you can open; remove it")
        # The endpoint comes from the authenticated Boat API, and up regenerates host keys after the first connection.
        return ["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15",
                "-i", str(self.private / "id_ed25519"), "-i", str(BOOTSTRAP / "id_ed25519"), "-o", "ControlMaster=auto", "-o", "ControlPersist=300",
                "-o", "ControlPath=" + self.control_path()]

    def control_path(self):
        return str(self.control / (hashlib.sha1(self.run["id"].encode()).hexdigest()[:8] + "-%C"))

    def exec(self, node, args, **kwargs):
        destination = ((self.record() or {}).get("sandboxes", {}).get(node["name"]) or {}).get("ssh")
        if not destination:
            raise RuntimeError(f"{node['name']} has no SSH endpoint yet")
        host, port = endpoint(destination)
        return self.commands(["ssh", *self.options(), "-p", port, "root@" + host, shlex.join(args)], **kwargs)

    def push(self, node, source, destination):
        directory = os.path.dirname(destination)
        if self.run["image"] == SNAPSHOT or os.path.getsize(source) < 1 << 20:
            with open(source, "rb") as content:
                self.exec(node, ["sh", "-ec", 'install -d "$1"; cat > "$2.push"; chmod 0755 "$2.push"; mv "$2.push" "$2"',
                                 "push", directory, destination], stdin=content, timeout=600)
            return
        # The upload from this host is the slow link; zstd cuts a release binary about fivefold.
        self.exec(node, ["sh", "-ec", 'install -d "$1"; zstd -dqc > "$2.push"; chmod 0755 "$2.push"; mv "$2.push" "$2"',
                         "push", directory, destination], input=compressed(source), timeout=600)

    def address(self, node):
        address = json.loads(self.boat("info", self.sandbox(node)["id"]).stdout)["sandbox"]["ip"]
        if not address:
            raise RuntimeError(f"{node['name']}: sandbox has no IPv6 address")
        return address

    def ssh(self, node):
        host, port = endpoint(self.sandbox(node)["ssh"])
        return f"root@{host}:{port}"

    def destroy(self):
        record = read(self.registry)
        if record is None:
            return
        if record["run"] != self.run["id"]:
            raise RuntimeError(f"Refusing to delete another run's sandboxes: {self.registry}")
        if POOL_SIZE and self.run.get("image") not in (None, SNAPSHOT):
            each(self.park, [name for name, entry in record["sandboxes"].items()
                             if entry.get("state") == "running" and entry.get("ssh") and not entry.get("discarded")])
            record = read(self.registry)
        failures = []
        for name, entry in record["sandboxes"].items():
            if entry.get("state") in GONE or not entry.get("id"):
                continue
            result = self.boat("delete", entry["id"], "--yes", check=False)
            failure = error(result) or {}
            if not result.returncode or failure.get("status") == 404:
                self.mark(name, state="deleted")
                continue
            if failure.get("status") != 403:
                failures.append(f"boat delete {entry['id']} failed: {failure or result.stderr.decode(errors='replace')}")
                continue
            if entry.get("state") != "stopped":
                stopped = self.boat("stop", entry["id"], check=False)
                if stopped.returncode:
                    failures.append(f"boat stop {entry['id']} failed: {error(stopped) or stopped.stderr.decode(errors='replace')}")
                    continue
                self.mark(name, state="stopped")
            print(f"Stopped {entry['id']} ({name}): this Boat key may not delete it; it needs the sandbox.delete-own scope "
                  f"and to have created the sandbox.", file=sys.stderr)
        if failures:
            raise RuntimeError("\n".join(failures))
        # Under the registry lock, so no update lands between the check and the unlink.
        with locked("registry.lock"):
            record = read(self.registry)
            if record and all(entry.get("state") in GONE for entry in record["sandboxes"].values()):
                self.registry.unlink()

    def park(self, name):
        """Reset a sandbox to its bake, prove it, and put it in the pool; anything unproven is left for delete."""
        node, entry, image = dict(name=name), self.sandbox(dict(name=name)), self.run["image"]
        expected = read(fingerprints(image))
        try:
            if not expected:
                raise RuntimeError("no fingerprint of this bake to prove it against")
            reset = self.exec(node, ["bash", "-ec", RESET, "reset", STASH, bootstrap()], check=False, timeout=300)
            if reset.returncode:
                raise RuntimeError(f"reset exited {reset.returncode}: {reset.stderr.decode(errors='replace').strip()[-300:]}")
            self.reboot(node)
            found = differences(expected, self.fingerprint(node))
            if found:
                raise RuntimeError("differs from a fresh clone in " + "; ".join(found))
            extended = self.boat("extend", entry["id"], "--ttl", TTL, check=False)
            if extended.returncode:
                raise RuntimeError(f"extend failed: {error(extended) or extended.stderr.decode(errors='replace').strip()}")
        except (RuntimeError, subprocess.TimeoutExpired) as why:
            print(f"Deleting {entry['id']} ({name}) instead of reusing it: {why}", file=sys.stderr)
            return
        try:
            with pool() as entries:
                dead = tidy(entries, sandbox_states(), registered())
                full = not self.make_room(entries, dead)
        except RuntimeError as why:
            print(f"Deleting {entry['id']} ({name}) instead of reusing it: {why}", file=sys.stderr)
            return
        retire(dead, lambda sandbox: self.boat("delete", sandbox, "--yes", check=False))
        if not full:
            with pool() as entries:
                # A sandbox Boat has not confirmed gone still counts, so a failed delete never lets the pool grow past its size.
                full = len(entries) >= POOL_SIZE
                if not full:
                    entries.append(dict(id=entry["id"], image=image, ip=entry["ip"], ssh=entry["ssh"], parked_at=int(time.time())))
                    self.mark(name, state="parked")
        if full:
            print(f"Deleting {entry['id']} ({name}): the pool already holds {POOL_SIZE} of this bake or newer ones.", file=sys.stderr)

    def make_room(self, entries, dead):
        """Whether the pool can take one more sandbox of this run's bake once `dead` is deleted, marking retiring, and adding
        to `dead`, what it displaces; park() admits it only after Boat confirms those deletes.

        All bakes together keep POOL_SIZE, so idle sandboxes never eat the account's active headroom. This bake displaces
        the oldest sandboxes of bakes the ledger made before it, never its own or a newer bake's."""
        image, order = self.run["image"], read(BAKES) or []
        rank = lambda bake: order.index(bake) if bake in order else -1
        live = [other for other in entries if not other.get("retiring")]
        older = sorted((other for other in live if rank(other["image"]) < rank(image)),
                       key=lambda other: (rank(other["image"]), other["parked_at"]))
        surplus = len(live) + 1 - POOL_SIZE
        if surplus > len(older):
            return False
        for other in older[:max(surplus, 0)]:
            other["retiring"] = True
            dead.append(other)
        return True

    def reboot(self, node):
        """Reboot the guest, which costs no Boat start, and wait until SSH answers from the new boot."""
        boot = self.exec(node, ["cat", "/proc/sys/kernel/random/boot_id"], timeout=30).stdout
        self.exec(node, ["sh", "-c", "sync; setsid sh -c 'sleep 1; echo b > /proc/sysrq-trigger' >/dev/null 2>&1 < /dev/null &"])
        host, port = endpoint(self.sandbox(node)["ssh"])
        time.sleep(3)
        self.commands(["ssh", *self.options(), "-p", port, "-O", "exit", "root@" + host], check=False)
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            result = self.exec(node, ["cat", "/proc/sys/kernel/random/boot_id"], check=False, timeout=15)
            if not result.returncode and result.stdout and result.stdout != boot:
                return
            if not result.returncode:
                time.sleep(1)
                self.commands(["ssh", *self.options(), "-p", port, "-O", "exit", "root@" + host], check=False)
            time.sleep(2)
        raise RuntimeError("SSH did not answer from a new boot within 240 s")

    def prepare(self):
        if not shutil.which(cli()[0]):
            raise RuntimeError(f"Boat CLI not found: {cli()[0]}; install it or set BOAT_CLI")
        if not shutil.which("zstd"):
            raise RuntimeError("zstd not found; install it")
        self.run["bootstrap_release"] = SNAPSHOT_RELEASE
        self.run["corrosion"] = corrosion_image(self.core)
        self.run["image"] = bake_name(self.run["corrosion"])
        # One baked snapshot per recipe; simultaneous runs don't bake it twice.
        with (REGISTRY / "bake.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            named = boat_json("snapshots")["named"]
            ours = read(BAKES) or []
            if any(snapshot["name"] == self.run["image"] and snapshot["status"] == "ready" for snapshot in named):
                return

            # Prune first; the account caps named snapshots.
            for name in stale_bakes(named, self.run["image"], ours, images_in_use()):
                if not self.boat("snapshot", "rm", name, check=False).returncode:
                    ours.remove(name)
                    forget_fingerprints(name)
            # The name hashes this host's bootstrap key, so it is ours even if the save outlives a timeout here.
            write(BAKES, list(dict.fromkeys([*ours, self.run["image"]])))
            print(f"Baking Boat snapshot {self.run['image']} (first run of this recipe only).", file=sys.stderr)
            bake = Provider(dict(id="bake-" + uuid.uuid4().hex[:10], manifest=str(BOOTSTRAP / "bake" / "cluster.json"),
                                 owner=self.run.get("owner", {}), image=SNAPSHOT, corrosion=self.run["corrosion"]),
                            self.commands, self.core)
            bake.private = BOOTSTRAP
            node = dict(name="bake")
            try:
                bake.create()
                bake.start(node)
                self.boat("snapshot", bake.sandbox(node)["id"], self.run["image"], timeout=1800)
            except BaseException:
                try:
                    bake.destroy()
                except (RuntimeError, OSError, subprocess.SubprocessError) as error:
                    print(f"Deleting the bake sandbox also failed: {error}", file=sys.stderr)
                raise
            bake.destroy()
