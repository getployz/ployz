import { describe, expect, it } from "vitest";
import type { Prep, StoredReport } from "#/modules/machines/waiting/checkin-wire";
import { type EnrollCandidate, type EnrollPlan, planServerEnrollment } from "#/modules/machines/waiting/enroll.server";
import { type EnrollInput, type EnrollStorage, isReadyFor } from "#/modules/machines/waiting/server-enrollment";

const report: StoredReport = {
  hostname: "web-a",
  os: { id: "ubuntu", version: "24.04" },
  kernel: "6.8.0",
  arch: "x86_64",
  virt: "kvm",
  cpuCount: 4,
  memoryTotalBytes: 8_589_934_592,
  disks: [],
  docker: { kind: "absent" },
  cgroup: "v2",
  zfs: { kind: "eligible", targets: [{ kind: "main_disk" }, { kind: "disk", byId: "/dev/disk/by-id/nvme-spare" }] },
  blockers: [],
  warnings: [],
  cliVersion: "0.3.0",
};

const row = (id: string, overrides: Partial<Omit<EnrollCandidate, "report">> & { readonly report?: Partial<StoredReport> } = {}): EnrollCandidate => ({
  id,
  fleetTokenId: "token-1",
  observedIp: "203.0.113.7",
  rejected: false,
  enrolling: false,
  tokenEnded: null,
  ...overrides,
  report: { ...report, hostname: `host-${id}`, ...overrides.report },
});

const input = (overrides: Partial<EnrollInput> = {}): EnrollInput => ({
  ids: ["a"],
  storage: { kind: "none" },
  names: { kind: "hostnames" },
  runs: { services: true, ingress: true, builds: false },
  ...overrides,
});

const empty = { hasFirst: false, takenNames: new Set<string>() };
const founded = { hasFirst: true, takenNames: new Set<string>() };

const planned = (plan: EnrollPlan) => {
  if (plan.kind !== "planned") throw new Error(`expected a plan, got: ${plan.message}`);
  return plan;
};

describe("founder choice", () => {
  it("makes the only selected row the founder of an empty Organization", () => {
    const plan = planned(planServerEnrollment(input(), [row("a")], empty));
    expect(plan.rows.map((r) => r.role)).toEqual(["founder"]);
  });

  it("refuses several rows in an empty Organization with no first server", () => {
    expect(planServerEnrollment(input({ ids: ["a", "b"] }), [row("a"), row("b")], empty)).toEqual({
      kind: "refused",
      message: "Pick the first server: it sets up the cluster the rest join.",
    });
  });

  it("puts the picked first server first and makes the rest joiners", () => {
    const plan = planned(planServerEnrollment(input({ ids: ["a", "b", "c"], first: "b" }), [row("a"), row("b"), row("c")], empty));
    expect(plan.rows.map((r) => [r.waitingServerId, r.role])).toEqual([["b", "founder"], ["a", "joiner"], ["c", "joiner"]]);
  });

  it("refuses a first server outside the selection, or one that is skipped", () => {
    expect(planServerEnrollment(input({ ids: ["a"], first: "z" }), [row("a")], empty)).toMatchObject({ kind: "refused" });
    expect(planServerEnrollment(input({ ids: ["a", "b"], first: "a" }), [row("a", { rejected: true }), row("b")], empty)).toEqual({
      kind: "refused",
      message: "The first server can't enroll: Not waiting.",
    });
  });

  it("picks no founder once the Organization is founded, and ignores first", () => {
    const plan = planned(planServerEnrollment(input({ ids: ["a", "b"], first: "a" }), [row("a"), row("b")], founded));
    expect(plan.rows.map((r) => r.role)).toEqual(["joiner", "joiner"]);
  });
});

describe("names", () => {
  it("uses each host's hostname", () => {
    const plan = planned(planServerEnrollment(input({ ids: ["a", "b"] }), [row("a"), row("b")], founded));
    expect(plan.rows.map((r) => r.instruction.name)).toEqual(["host-a", "host-b"]);
  });

  it("plans the Machine Name a hostname normalises to, else one from the waiting server's id", () => {
    const id = "0f1e2d3c-4b5a-4968-8776-655443322110";
    const plan = planned(planServerEnrollment(
      input({ ids: ["a", id] }),
      [row("a", { report: { hostname: "MY-SERVER" } }), row(id, { report: { hostname: "___" } })],
      founded,
    ));
    expect(plan.rows.map((r) => [r.hostname, r.instruction.name])).toEqual([["MY-SERVER", "my-server"], ["___", "server-0f1e2d3c"]]);
  });

  it("numbers a pattern from the founder down", () => {
    const plan = planned(planServerEnrollment(
      input({ ids: ["a", "b", "c"], first: "c", names: { kind: "pattern", pattern: "web-{n}" } }),
      [row("a"), row("b"), row("c")],
      empty,
    ));
    expect(plan.rows.map((r) => [r.waitingServerId, r.instruction.name])).toEqual([["c", "web-1"], ["a", "web-2"], ["b", "web-3"]]);
  });

  it("refuses a pattern without {n} for more than one server, and takes it for one", () => {
    const names = { kind: "pattern", pattern: "web" } as const;
    expect(planServerEnrollment(input({ ids: ["a", "b"], names }), [row("a"), row("b")], founded)).toEqual({
      kind: "refused",
      message: "A name pattern needs {n} to name more than one server.",
    });
    expect(planned(planServerEnrollment(input({ names }), [row("a")], founded)).rows[0]?.instruction.name).toBe("web");
  });

  it("numbers a hostname another Server or this batch already holds, as ployz server add refuses it", () => {
    const plan = planned(planServerEnrollment(
      input({ ids: ["a", "b", "c"] }),
      [row("a", { report: { hostname: "MY-SERVER" } }), row("b", { report: { hostname: "my-server" } }), row("c")],
      { hasFirst: true, takenNames: new Set(["my-server", "host-c-2"]) },
    ));
    expect(plan.rows.map((r) => r.instruction.name)).toEqual(["my-server-2", "my-server-3", "host-c"]);
  });

  it("keeps a numbered name within 63 characters", () => {
    const long = "a".repeat(63);
    const plan = planned(planServerEnrollment(
      input({ ids: ["a"] }), [row("a", { report: { hostname: long } })], { hasFirst: true, takenNames: new Set([long]) },
    ));
    expect(plan.rows[0]?.instruction.name).toBe(`${"a".repeat(61)}-2`);
  });

  it("skips the numbers a pattern would repeat, and numbers a taken pattern without {n}", () => {
    const takenNames = new Set(["web-1", "web-3", "web"]);
    const numbered = planned(planServerEnrollment(
      input({ ids: ["a", "b"], names: { kind: "pattern", pattern: "web-{n}" } }), [row("a"), row("b")], { hasFirst: true, takenNames },
    ));
    expect(numbered.rows.map((r) => r.instruction.name)).toEqual(["web-2", "web-4"]);
    const single = planned(planServerEnrollment(
      input({ names: { kind: "pattern", pattern: "web" } }), [row("a")], { hasFirst: true, takenNames },
    ));
    expect(single.rows[0]?.instruction.name).toBe("web-2");
  });
});

describe("storage", () => {
  const zfs = (useSpareDisk: boolean, fallbackToDocker: boolean) => ({ kind: "zfs", useSpareDisk, fallbackToDocker }) as const;
  const ineligible = { zfs: { kind: "ineligible", code: "no_disk", message: "No empty disk." } } as const;
  const storageOf = (storage: EnrollInput["storage"], candidate: EnrollCandidate) =>
    planServerEnrollment(input({ storage }), [candidate], founded);

  it("passes Docker volumes through", () => {
    expect(planned(storageOf({ kind: "none" }, row("a"))).rows[0]?.instruction.storage).toEqual({ kind: "none" });
  });

  it("names the spare disk only when asked to use it", () => {
    expect(planned(storageOf(zfs(true, false), row("a"))).rows[0]?.instruction.storage)
      .toEqual({ kind: "zfs", device: "/dev/disk/by-id/nvme-spare" });
    expect(planned(storageOf(zfs(false, false), row("a"))).rows[0]?.instruction.storage).toEqual({ kind: "zfs" });
  });

  it("uses the main disk when there is no spare one", () => {
    const mainOnly = row("a", { report: { zfs: { kind: "eligible", targets: [{ kind: "main_disk" }] } } });
    expect(planned(storageOf(zfs(true, false), mainOnly)).rows[0]?.instruction.storage).toEqual({ kind: "zfs" });
  });

  it("resolves the by-id basename reported by the host to its device path", () => {
    const spare = row("a", { report: { zfs: { kind: "eligible", targets: [{ kind: "disk", byId: "virtio-spare" }] } } });
    expect(planned(storageOf(zfs(true, false), spare)).rows[0]?.instruction.storage)
      .toEqual({ kind: "zfs", device: "/dev/disk/by-id/virtio-spare" });
  });

  it("falls back to Docker volumes where ZFS can't run, or skips the row with ZFS's reason", () => {
    expect(planned(storageOf(zfs(true, true), row("a", { report: ineligible }))).rows[0]?.instruction.storage).toEqual({ kind: "none" });
    const plan = planned(storageOf(zfs(true, false), row("a", { report: ineligible })));
    expect(plan.rows).toEqual([]);
    expect(plan.skipped).toEqual([{ id: "a", reason: "No empty disk." }]);
  });

  it("plans ZFS on a host whose build is still running", () => {
    const building = row("a", { report: { prep: { docker: "ready", zfs: "installing" } } });
    expect(planned(storageOf(zfs(false, false), building)).rows[0]?.instruction.storage).toEqual({ kind: "zfs" });
  });
});

describe("skips", () => {
  const reasonFor = (candidate: EnrollCandidate | null) =>
    planned(planServerEnrollment(input({ ids: ["a"] }), candidate === null ? [] : [candidate], founded)).skipped;

  it.each([
    ["an unknown id", null, "Not waiting"],
    ["a rejected host", row("a", { rejected: true }), "Not waiting"],
    ["a host already enrolling", row("a", { enrolling: true }), "Already enrolling"],
    ["a host on an expired token", row("a", { tokenEnded: "expired" }), "Fleet Token expired"],
    ["a host on a revoked token", row("a", { tokenEnded: "revoked" }), "Fleet Token revoked"],
    ["a blocked host", row("a", { report: { blockers: [{ code: "disk", message: "Disk is 20 GB. Ployz needs 30 GB." }] } }), "Disk is 20 GB. Ployz needs 30 GB."],
    ["a CLI from before enroll", row("a", { report: { cliVersion: "0.3.0-beta.5" } }), "Update ployz"],
    ["an older release", row("a", { report: { cliVersion: "0.2.2" } }), "Update ployz"],
    ["a host whose CLI never said its version", row("a", { report: { cliVersion: undefined } }), "Update ployz"],
  ])("skips %s", (_, candidate, reason) => {
    expect(reasonFor(candidate)).toEqual([{ id: "a", reason }]);
  });

  it("keeps a host still preparing", () => {
    expect(reasonFor(row("a", { report: { prep: { docker: "installing", zfs: "installing" } } }))).toEqual([]);
  });

  it.each(["0.3.0-beta.6", "0.3.0", "0.3.1", "0.4.0"])("keeps a host whose CLI is %s", (cliVersion) => {
    expect(reasonFor(row("a", { report: { cliVersion } }))).toEqual([]);
  });

  it("plans each id once", () => {
    expect(planned(planServerEnrollment(input({ ids: ["a", "a"] }), [row("a")], founded)).rows).toHaveLength(1);
  });
});

it("counts distinct providers and /16 networks", () => {
  const plan = planned(planServerEnrollment(input({ ids: ["a", "b", "c"] }), [
    row("a", { observedIp: "203.0.113.7", report: { provider: "hetzner" } }),
    row("b", { observedIp: "203.0.200.9", report: { provider: "hetzner" } }),
    row("c", { observedIp: "::ffff:198.51.100.4" }),
  ], founded));
  expect(plan.spread).toEqual({ providers: 2, networks: 2 });
});

describe("readiness", () => {
  const none: EnrollStorage = { kind: "none" };
  const zfs: EnrollStorage = { kind: "zfs" };
  const ready = (prep: Prep | undefined, storage: EnrollStorage) =>
    isReadyFor(prep === undefined ? report : { ...report, prep }, storage);

  it.each([
    [undefined, true, true],
    [{ docker: "installing", zfs: "installing" }, false, false],
    [{ docker: "failed", zfs: "off" }, false, false],
    [{ docker: "ready", zfs: "installing" }, true, false],
    [{ docker: "ready", zfs: "ready" }, true, true],
    [{ docker: "ready", zfs: "failed" }, true, false],
    [{ docker: "ready", zfs: "skipped" }, true, false],
    [{ docker: "ready", zfs: "off" }, true, false],
  ] as const)("prep %j is ready for none: %s, for zfs: %s", (prep, forNone, forZfs) => {
    expect(ready(prep, none)).toBe(forNone);
    expect(ready(prep, zfs)).toBe(forZfs);
  });

  it("holds a host with a blocker whatever its prep", () => {
    const blocked = { ...report, prep: { docker: "ready", zfs: "ready" }, blockers: [{ code: "x", message: "x" }] } as const;
    expect(isReadyFor(blocked, none)).toBe(false);
    expect(isReadyFor(blocked, zfs)).toBe(false);
  });
});
