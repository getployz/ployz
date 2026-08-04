# Corrosion three-node spike

> **PROTOTYPE — do not merge.** This is throwaway evidence for the Wayfinder
> ticket “Spike: three-node Corrosion cluster under Ployz-shaped load.”

## Question

Is stock Corrosion v1, run as a small systemd sidecar with gossip confined to a
WireGuard mesh, simpler to operate and recover than the control-plane machinery
it would replace?

The useful result is an operator verdict, not a benchmark trophy. Propagation,
subscriptions, storage growth, recovery, schema skew, reseeding, and resource
use are measured so the operator can judge how many concepts and repair steps
the cluster demands.

The load schema is deliberately provisional. It borrows Uncloud's small
JSON-blob-table shape to exercise realistic row sizes without deciding Ployz's
production row model.

The completed certification and proposed verdict are in
[RESULTS.md](RESULTS.md).

## Run

The certification run creates three temporary 1 GiB Vultr instances in one
region, installs real WireGuard and systemd, captures evidence, and deletes
exactly the resources it created. At the current plan price the three hosts
cost USD 0.021/hour combined.

```bash
VULTR_API_KEY=... testing/corrosion-spike/run.sh
```

The API key is read from the environment and is never copied to a host or
written to evidence. The runner prints its local run directory immediately.

During development, a failed run can retain its exact hosts and resume at the
failed phase. This avoids reprovisioning or repeating successful drills:

```bash
VULTR_API_KEY=... testing/corrosion-spike/run.sh resume \
  .scratch/corrosion-spike/<run-id> <phase>
```

The resume path never deletes hosts automatically. The final certification uses
the no-argument command above on fresh hosts.

If a run is interrupted after provisioning, use the recorded state to remove
only that run's instances and temporary Vultr SSH key:

```bash
VULTR_API_KEY=... testing/corrosion-spike/run.sh cleanup \
  .scratch/corrosion-spike/<run-id>
```

Evidence lands under the run directory. `evidence/report.md` is the human
summary; JSON samples, journals, WireGuard state, database measurements, and
the exact created-resource ids remain beside it.

## Fixed inputs and limits

The runner reads its operational release pin from
[`corrosion-release.json`](../../corrosion-release.json). The values below
record the completed certification evidence.

- Stock release: GitHub `v1.0.0`, x86_64 Linux asset SHA-256
  `3504d7d1b4b53737457fc40f2353a400cf4df0c1217ec318924d7ee310876194`.
- The verified release asset currently reports its embedded version as
  `0.2.0-beta.0`; the report preserves that packaging mismatch.
- Corrosion v1.0.0 is the only published stable v1 release, so this run cannot
  certify mixed-version rolling upgrades. It rehearses same-version
  replacement and the v1 reseed escape hatch without inventing a v0 migration.
- Three same-region VPSes exercise real host supervision, reboot, storage, and
  WireGuard behavior. They do not certify cross-region WAN latency.
- Vultr requires at least 1 GiB RAM for its Ubuntu LTS images, so this is the
  cheapest supported Ubuntu profile rather than a 512 MiB result.
- The subscription client follows the deliberately simple production pattern:
  events invalidate, every event triggers a full query, and lost replay state
  restarts and resnapshots.
