# Ployz feature map

Every user-visible feature of Ployz, how to reach it and what result proves it works. A verifier swarm splits a release's verification into the [slices](slices.md) at the bottom of this index, one slice per verifier thread on one fresh Boat Cluster. Gap ≠ pass: an entry a verifier couldn't drive is reported as not run, never as passing.

`docs/user/` is the spec. An entry marked **undocumented** is shipped behavior that no `docs/user/` page describes: verify it against the code and its `--help`, and treat a missing page as a docs finding.

## Areas

| File | Prefix | What it covers |
| --- | --- | --- |
| [account.md](account.md) | ACC | Sign-in, Organizations, members and roles, tokens, approvals, billing, the sandbox claim |
| [cli-and-agents.md](cli-and-agents.md) | CLI, AGT | CLI plumbing (contexts, links, status, docs, settings catalog) and the agent surface: `ployz mcp`, remote MCP, the Agent sidebar |
| [install.md](install.md) | INS | Install line, cloud-init, uninstall, self-hosted Cloud |
| [servers.md](servers.md) | SRV | Adding, approving, upgrading, draining, removing and recovering Servers; devices, keys and regions |
| [services.md](services.md) | SVC | Services, settings, variables, Configs, databases, scaling, runtime control |
| [environments.md](environments.md) | ENV | Projects, Environments, Branches, Sync, PR Environments |
| [deploy.md](deploy.md) | DEP | Staging, Deploy, Deployments, history and rollback, zero-downtime |
| [builds.md](builds.md) | BLD | Builds on Servers and GitHub Actions, GitHub connection, build order |
| [networking.md](networking.md) | NET | Domains, certificates, the Ingress Proxy, Cloudflare Tunnel, private networking, Private Access |
| [volumes.md](volumes.md) | VOL | Managed and Docker Volumes, resize, Moves, Mirrors, restore, Trash, leftovers |
| [observe.md](observe.md) | OBS | Logs, metrics, uptime, analytics, traces |

## Entry format

Each entry is one `###` heading with a stable id and a fixed list of fields, so `grep` finds a slice of the map:

```bash
grep -h '^### ' .agents/feature-map/*.md                                        # every entry
grep -l 'Surface:\*\* dashboard' .agents/feature-map/*.md                       # areas with dashboard entries
grep -B2 -A6 'Compat:\*\* CLI→daemon' .agents/feature-map/*.md                  # what crosses the CLI–daemon boundary
grep -h -B7 'Docs:\*\*.*undocumented' .agents/feature-map/*.md | grep '^###'   # docs gaps
```

```markdown
### VOL-16 Deleted data waits in the Server's trash for 7 days
- **Surface:** daemon
- **Reach:** `ployz volume rm data`, then `ployz deploy --accept-volume-loss data --expect-version <v>` (the refusal names `<v>`); restart `ployzd`; power the Server off for a while; advance past 7 days of uptime
- **Proof:** `zfs list` shows the dataset moved from `ployz/ployz/<project>-<env>_vol-<id>` to `ployz/ployz-trash/<entry id>`; `sudo ployz volume trash ls` lists the entry with DELETED and EXPIRES 7 days later, which moves only while the Server runs; `zfs list` still counts its space; after 7 days of uptime the entry and dataset are gone
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server upgrade: the sweep must survive a 0.4 daemon restart; 0.3.19 deletes at once
- **Docs:** services/volumes.md#delete-a-volume
```

- **Surface:** CLI, dashboard, MCP, install or daemon. Daemon means behavior a Server shows on its own, without a command driving it at the time (restarts, certificate renewal, upgrades on a channel, keeping data).
- **Reach:** the exact command, route or click path. Routes are relative to the Cloud origin, with the verify-dashboard seed's names (`/cloud/ada/shop/production`).
- **Proof:** an observable result a verifier can read and compare: an output field, an HTTP answer, a file's contents, a ZFS dataset, a row on a page after reload. "It runs" or "no error" is never proof.
- **Needs:** Machine count, Volume kind (none, Managed on ZFS, or Docker), and whether Cloud is needed. Cloud is never prod Cloud (ployz.dev): Standalone, `verify-cluster up --cloud` or verify-dashboard's `SERVERS=N` local Cloud. `External:` names anything the lab lacks, such as a real DNS name, a Cloudflare or Tailscale account, or GitHub.
- **Drive:** which skill drives it: `verify-server` (real Machines through `core/scripts/verify-cluster`), `verify-dashboard` (browser; `SERVERS=1` for runtime UI), and the `verify-*` Stateright model that checks its ordering, when one exists.
- **Compat:** whether the feature crosses the CLI–daemon, CLI–Cloud, Cloud–Server or Server–Server boundary, or a Server upgrade, so the 0.3.19 ↔ 0.4 matrix can pick it up. `none` when it stays inside one component.
- **Docs:** the `docs/user/` page and section it comes from, or **undocumented**.

## Driving entries from an agent

What the three proof runs that shipped with this map (CLI SVC-46, daemon VOL-16 to VOL-18, dashboard SRV-37) taught:

- A checkout or 0.4 CLI run by an agent needs `--why "<goal>"` before the subcommand. A 0.3.19 CLI rejects `--why`, so leave it off on release-only runs.
- A Deploy that ships a staged change signs it on this device and asks a person's yes at a terminal; `--yes` only ships `--image` changes. `verify-cluster cli` gives the CLI no TTY, so run that CLI under `script` with `TERM=xterm` (an agent's shell often sets `TERM=dumb`, which turns prompts off) and the run's environment from `cli_environment` in `core/scripts/verify-cluster`: `PLOYZ_CONFIG=<run>/private/config.yaml`, `PLOYZ_CONTEXT=verify` and `PLOYZ_STORE=sqlite:<run>/private/store.sqlite` on a Standalone run, from `<run>/workspace`. Type `y` at **Approve … ?**; a Deploy that deletes Volume data also needs `--accept-volume-loss <name> --expect-version <v>`, which its refusal names.
- On a Standalone run, `ployz exec` works without approving a device; on a `--cloud` run use `cli --device`.

## Keep it current

A PR that adds, removes or changes a user-visible feature updates its entry in the same PR, as it updates `docs/user/`. A verifier that finds an entry's Reach or Proof wrong fixes the entry in its findings PR.

## Slices

See [slices.md](slices.md).
