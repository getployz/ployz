# Ployz v1 Todos

Captured from product notes. Grouped by area; not yet prioritized or
scheduled — see `v1-roadmap.md` for sequencing.

## Config

- Domain management
- Pre-deploy command
- Post-start command
- Pre-start command

## Sharing projects

- Share a project into a different workspace (uses that workspace's compute)

## ZFS storage management

- Send to target node
- Boot from snapshot
- Auto deletion

## Templates with sub-managed services

- Opt in/out of restore on node death
- Opt in/out of container replacement
- Recommend containers to kill on scale down
- Always expose replicas var
- Expose env vars

## Backups

- (Top-level item, no sub-points yet)

## Multi-WireGuard interfaces per node

Motivation: help determine lowest-latency / cheapest link.

- Continuously monitor ping
- Prefer local IPs
- Faster connect on boot

## Node hardening and security updates

- Security update strategy (TBD)
- Recommended OS

## Node-unavailable plans

- Plan most operations around needing a majority
- Allow deletion of offline servers

## Workspace operations

- Migrating projects between workspaces

## Basic dashboard linking

CLI/daemon surfaces that must be reachable from the dashboard:

- Server init
- Server add
- Server drain
- Server remove
- Deploy
- Delete namespace
- Cluster snapshot
- Cluster snapshot streaming

## Canvas dual mode

- Service editor mode
- Template editor mode (edit how the project responds to PRs, etc.)

## PR environments

- Add PR config per service
- Two types: clone or fresh
- Scale down replicas by default
- Node substitution (open question: cleanly sharing e.g. Postgres between
  prod and fork envs)
- Sync changes back into prod
- Env var substitution
- Creation hooks
- Open question: is a service per-namespace with each PR env as an overlay?

## Roles

- Admin
- Viewer
- Billing
