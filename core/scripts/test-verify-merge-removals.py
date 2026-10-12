#!/usr/bin/env python3
"""Check on one real Machine that a PR merge removes the Services its PR Environment deleted and never a Volume's
data: production keeps pg-data, its Docker volume and its dataset, and an Undo of the merge brings web and db back
onto the same rows.

Usage: test-verify-merge-removals.py [daemon] --cloud. dashboard/scripts/verify/merge-removals.ts tells the Store
what a fake GitHub App would: the fixture, PR 501 opening and merging, and the push of its merge commit."""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.dont_write_bytecode = True
core = Path(__file__).resolve().parents[1]
dashboard = core.parent / "dashboard"
helper = core / "scripts" / "verify-cluster"
positional = [arg for arg in sys.argv[1:] if arg != "--cloud"]
assert "--cloud" in sys.argv[1:], "A PR Environment needs Cloud: pass --cloud"
daemon = positional[0] if positional else "stable"
result = subprocess.run([helper, "up", "--machines", "1", "--daemon", daemon, "--cloud"],
                        cwd=core, stdout=subprocess.PIPE, text=True, check=True)
manifest = Path(result.stdout.strip())
evidence = manifest.parent / "evidence"
SENTINEL = "1|alpha\n2|beta\n3|gamma"


def verify(*args, check=True):
    return subprocess.run([helper, *map(str, args)], check=check, stdout=subprocess.PIPE, text=True)


def cli(*args, env="production"):
    return verify("cli", "--timeout", "600", manifest, "--", *args, "--env", env).stdout


def view(*args, env="production"):
    return json.loads(cli("--json", *args, env=env))


def guest(*args):
    return verify("exec", manifest, "machine-1", "--", *args).stdout


def keep(name, value):
    (evidence / f"merge-removals-{name}.json").write_text(json.dumps(value, indent=2))
    return value


def wait(condition, seconds, message):
    deadline = time.monotonic() + seconds
    while not (value := condition()):
        assert time.monotonic() < deadline, message
        time.sleep(1)
    return value


def fixture(phase, organization):
    script = "set -a; . .verify/run/env; set +a; exec node scripts/verify/runner.mjs /scripts/verify/merge-removals.ts"
    environment = dict(os.environ, MERGE_REMOVALS_PHASE=phase, MERGE_REMOVALS_ORGANIZATION=organization)
    ran = subprocess.run(["bash", "-c", script], cwd=dashboard, env=environment, capture_output=True, text=True)
    lines = [line for line in ran.stdout.splitlines() if line.startswith("MERGE_REMOVALS ")]
    assert ran.returncode == 0 and lines, f"merge-removals.ts {phase} failed:\n" + "\n".join(ran.stderr.splitlines()[-40:])
    return keep(f"fixture-{phase}", json.loads(lines[-1].removeprefix("MERGE_REMOVALS ")))


def psql(sql):
    ran = verify("cli", "--timeout", "120", manifest, "--", "exec", "-T", "db", "--env", "production", "--",
                 "psql", "-X", "-q", "-At", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-c", sql, check=False)
    return ran.stdout.strip() if ran.returncode == 0 else None


def sentinel():
    return wait(lambda: psql("SELECT id, note FROM sentinel ORDER BY id"), 300, "db never answered the sentinel query")


def services(env="production"):
    return {listing["name"]: listing for listing in view("service", "ls", env=env)["services"]}


def containers():
    """Every container on the Machine by service name: the Docker id, its state and when it started."""
    lines = guest("docker", "ps", "-a", "--no-trunc", "--filter", "label=ployz.service.name", "--format",
                  '{{.ID}} {{.Label "ployz.service.name"}} {{.State}}').splitlines()
    found = {}
    for line in lines:
        container, name, state = line.split()
        started = guest("docker", "inspect", "-f", "{{.State.StartedAt}}", container).strip()
        found.setdefault(name, []).append(dict(id=container, state=state, started=started))
    return found


def mounted(container):
    mounts = json.loads(guest("docker", "inspect", "-f", "{{json .Mounts}}", container))
    return next(mount.get("Name") or mount["Source"] for mount in mounts if mount["Destination"] == "/var/lib/postgresql/data")


def storage(volume):
    """pg-data as the Machine holds it: its Docker volume and, where the Machine has ZFS, its dataset's guid."""
    docker = json.loads(guest("docker", "volume", "inspect", volume))[0]
    datasets = verify("exec", manifest, "machine-1", "--", "zfs", "list", "-H", "-o", "name,guid", check=False)
    guids = [line.split()[1] for line in datasets.stdout.splitlines() if line.split()[0].endswith("/" + volume)]
    return dict(name=volume, driver=docker["Driver"], created=docker["CreatedAt"], mountpoint=docker["Mountpoint"],
                dataset=guids)


def verdicts(on_merge):
    """Each node row of On merge as its LANDS column reads, keyed by node: the copy the CLI and the check show."""
    words = {}
    for destination in on_merge["destinations"]:
        for row in destination["rows"]:
            if row.get("name") is not None:
                continue
            if row["ride"] == "kept":
                word = "Kept"
            elif row["ride"] == "excluded":
                word = "Excluded"
            elif row.get("refused_by"):
                word = "In use"
            elif row["ride"] in ("rides", "edited") and row["lands"] is None:
                word = f"Removes {row['node']}"
            else:
                word = row["ride"]
            words[row["node"]] = word
    return words


try:
    run = json.loads(manifest.read_text())
    organization = run["cloud"]["organization"]
    fixture("setup", organization)
    verify("cli", "--timeout", "600", manifest, "--", "link", "--project", "removals")
    cli("deploy", "web", "db", "worker")
    wait(lambda: psql("SELECT 1") == "1", 300, "db never accepted connections")
    psql("CREATE TABLE sentinel (id int PRIMARY KEY, note text NOT NULL); "
         "INSERT INTO sentinel VALUES (1, 'alpha'), (2, 'beta'), (3, 'gamma')")
    assert sentinel() == SENTINEL, "The sentinel rows read back as written"

    before = keep("production-before", dict(
        services={name: listing["id"] for name, listing in services().items()},
        volume=view("volume", "inspect", "pg-data")["id"],
        containers=containers(),
        revision=max(revision["revision"] for revision in view("history")["revisions"]),
    ))
    assert {"web", "db", "worker", "anchor"} <= before["services"].keys(), before["services"]
    assert len(before["containers"].get("db", [])) == 1, "One db container runs before the merge"
    pg_data = keep("storage-before", storage(mounted(before["containers"]["db"][0]["id"])))

    opened = fixture("open", organization)
    assert services("pr-501").keys() >= {"web", "db"}, "PR 501 opened a PR Environment with web and db copied"
    for service in ("web", "db"):
        cli("service", "rm", service, env="pr-501")
    cli("volume", "rm", "pg-data", env="pr-501")

    on_merge = keep("on-merge", view("env", "on-merge", env="pr-501"))
    words = verdicts(on_merge)
    assert words.get("web") == "Removes web", f"On merge removes web: {words}"
    assert words.get("db") == "Removes db", f"On merge removes db: {words}"
    assert words.get("volumes.pg-data") == "Kept", f"On merge keeps pg-data: {words}"
    assert "worker" not in words, f"On merge leaves the uncopied worker alone: {words}"
    table = cli("env", "on-merge", env="pr-501")
    for word in ("Removes web", "Removes db", "Kept"):
        assert word in table, f"The CLI's On merge says {word!r}:\n{table}"

    web_row = next(row["row"] for into in on_merge["destinations"] for row in into["rows"]
                   if row["node"] == "web" and row.get("name") is None)
    cli("env", "never-sync", web_row, env="pr-501")
    assert verdicts(view("env", "on-merge", env="pr-501")).get("web") == "Excluded", "Never sync excludes web's removal"
    cli("env", "never-sync", web_row, "--off", env="pr-501")
    assert verdicts(view("env", "on-merge", env="pr-501")) == words, "Syncing web again restores the removal"

    merged = fixture("merge", organization)
    shown = {deployment: view("deployment", "show", deployment) for deployment in merged["admitted"]}
    production = [deployment for deployment, found in shown.items() if found["environment"]["name"] == "production"]
    assert len(production) == 1, f"The merge admits one production Deployment: {shown}"
    deployment = wait(lambda: (found := view("deployment", "show", production[0]))["status"] in
                      ("applied", "failed", "cancelled", "unknown") and found, 900, "The merge Deployment never finished")
    keep("merge-deployment", deployment)
    assert deployment["status"] == "applied", f"The merge Deployment applied: {deployment['status']}"
    assert not deployment["remove"], "The merge Deployment is not an Environment removal"
    nodes = {(node["type"], node["name"]): node["outcome"] for node in deployment["nodes"]}
    assert nodes.get(("service", "web")) == "removed", f"The merge Deployment removes web: {nodes}"
    assert nodes.get(("service", "db")) == "removed", f"The merge Deployment removes db: {nodes}"
    assert not [key for key, outcome in nodes.items() if key[0] == "volume" and outcome == "removed"], \
        f"The merge Deployment deletes no Volume: {nodes}"

    after = services()
    assert not {"web", "db"} & after.keys(), f"web and db left production's Saved and Working: {list(after)}"
    assert view("diff")["draft_count"] == 0, "Production's Working matches Saved after the merge"
    now = keep("containers-after-merge", containers())
    assert not {"web", "db"} & now.keys(), f"Every web and db container is gone from the Machine: {list(now)}"
    assert now.get("worker") == before["containers"]["worker"], "worker's container is untouched"
    assert view("volume", "inspect", "pg-data")["id"] == before["volume"], "pg-data keeps its resource id"
    assert storage(pg_data["name"]) == pg_data, "pg-data keeps its Docker volume and dataset"

    revisions = [revision["revision"] for revision in view("history")["revisions"]]
    merge_revision = max(revisions)
    assert merge_revision > before["revision"], "The merge saved a new production revision"
    cli("history", "undo", str(merge_revision))
    restored = services()
    assert {name: restored[name]["id"] for name in ("web", "db")} == \
        {name: before["services"][name] for name in ("web", "db")}, "Undo restores web and db with their original ids"
    cli("publish")
    cli("deploy", "web", "db")
    again = keep("containers-after-undo", containers())
    assert again.get("worker") == before["containers"]["worker"], "worker's container is untouched by the Undo deploy"
    assert mounted(again["db"][0]["id"]) == pg_data["name"], "db mounts the same pg-data Docker volume again"
    assert view("volume", "inspect", "pg-data")["id"] == before["volume"], "pg-data's resource id survived the Undo"
    assert storage(pg_data["name"]) == pg_data, "pg-data's Docker volume and dataset survived the Undo"
    assert sentinel() == SENTINEL, "The sentinel rows survived the merge exactly"
    print(json.dumps(dict(daemon=daemon, evidence=str(evidence), merge_revision=merge_revision,
                          cancelled_previews=len(opened["cancelled"]), deployment=production[0])))
except BaseException:
    verify("doctor", manifest, check=False)
    raise
finally:
    verify("down", manifest)
