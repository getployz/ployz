# ployz

**ployz** is a modern machine network control plane for secure, automated cluster networking and coordination.

## Architecture (current)

The active code path is intentionally split by responsibility:

- `cmd/ployz`: operator CLI.
- `cmd/ployzd`: daemon entrypoint.
- `daemon`: unix-socket gRPC server and daemon runtime shell.
- `machine`: local machine identity/state lifecycle; mesh attachment is optional.
- `machine/mesh`: network stack lifecycle (WireGuard, store runtime, convergence).
- `machine/convergence`: event-driven peer reconciliation loop.
- `infra/*`: concrete adapters for external systems (WireGuard, Corrosion, SQLite, etc.).
- `platform/*`: compile-time OS split (`linux`, `darwin`, fallback `stub`) and defaults.

Dependency direction is one-way: `cmd/*` -> `daemon` -> `machine` -> `machine/*` -> `infra/*`.
`platform/*` selects platform-specific concrete implementations at the wiring edge.

Historical code is kept under `_internal_legacy_do_not_read/` for reference only and is
not part of the active package graph.

## Quick Install

Install the latest stable release with a single command (works for Linux or macOS):

```sh
curl -fsSL https://github.com/getployz/ployz/releases/latest/download/install.sh | sudo sh
```

Or with wget:

```sh
wget -qO- https://github.com/getployz/ployz/releases/latest/download/install.sh | sudo sh
```

- For details, [see the install script](https://github.com/getployz/ployz/releases/latest/download/install.sh).

> **Note:** The installer requires root privileges and will auto-detect your OS/architecture, verify checksums, install necessary binaries and Docker, and enable relevant services.  
> On macOS, Docker must be installed manually (see [OrbStack](https://orbstack.dev)).  
> See the script for details and options.

---

## Manual installation

- Download release binaries or packages from the [Releases page](https://github.com/getployz/ployz/releases).
- For `.deb`/`.rpm`, install with your distro's package manager.
- For macOS, extract and copy the binaries from the `.tar.gz` archive.

---

## Usage

After installing, run:

```sh
ployz --help
```
for usage and commands.

---

For more, see the [documentation](https://github.com/getployz/ployz).

## References

- Corrosion readiness and metrics reference: [`docs/corrosion-health.md`](docs/corrosion-health.md)
