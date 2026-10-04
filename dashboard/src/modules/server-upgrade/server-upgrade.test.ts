import { describe, expect, it } from "vitest";
import {
  compareVersions,
  newMajorLine,
  releaseFromPointer,
  releaseLine,
  serverUpgradeLine,
  serversUpgradeLine,
  UPGRADE_OBSERVATION_LIMIT_MS,
  type LatestUpgrade,
} from "./server-upgrade";

describe("compareVersions", () => {
  it("orders the published forms as semver, a beta before its release", () => {
    expect(compareVersions("0.2.1", "0.2.2")).toBeLessThan(0);
    expect(compareVersions("0.10.0", "0.9.9")).toBeGreaterThan(0);
    expect(compareVersions("1.2.3", "1.2.3")).toBe(0);
    expect(compareVersions("1.2.3-beta.9", "1.2.3")).toBeLessThan(0);
    expect(compareVersions("1.2.3-beta.10", "1.2.3-beta.2")).toBeGreaterThan(0);
  });

  it("puts a beta behind its release and ahead of the release before", () => {
    expect(compareVersions("0.2.3-beta.1", "0.2.3")).toBeLessThan(0);
    expect(compareVersions("0.2.3-beta.1", "0.2.2")).toBeGreaterThan(0);
  });

  it("can't compare an unknown or unpublished version", () => {
    expect(compareVersions("", "0.2.2")).toBeNull();
    expect(compareVersions("0.2.2-rc.1", "0.2.2")).toBeNull();
    expect(compareVersions("v0.2.2", "0.2.2")).toBeNull();
  });
});

describe("Release Channel pointers", () => {
  it("read `vX.Y.Z` on one line", () => {
    expect(releaseFromPointer("v0.2.2\n")).toBe("0.2.2");
    expect(releaseFromPointer("v1.0.0-beta.3\n")).toBe("1.0.0-beta.3");
    expect(releaseFromPointer("<html>")).toBeNull();
  });

  it("are scoped to the Server's release line", () => {
    expect(releaseLine("0.2.1")).toBe("v0");
    expect(releaseLine("12.0.0-beta.1")).toBe("v12");
    expect(releaseLine("")).toBeNull();
  });
});

const NOW = Date.parse("2026-10-04T12:00:00.000Z");
const attempt = (overrides: Partial<LatestUpgrade> = {}): LatestUpgrade => ({
  attemptId: "a".repeat(32),
  outcome: "running",
  stage: null,
  error: null,
  fromVersion: "0.2.1",
  targetVersion: "0.2.2",
  startedAt: new Date(NOW - 60_000).toISOString(),
  ...overrides,
});
const line = (input: Partial<Parameters<typeof serverUpgradeLine>[0]> = {}) => serverUpgradeLine({
  version: "0.2.1", status: "online", release: "0.2.2", latest: null, pendingFrom: undefined, now: NOW,
  automatic: true, rolloutRunning: false, ...input,
});

describe("serverUpgradeLine", () => {
  it("offers the release only to an online, idle Server running an older one", () => {
    expect(line()).toEqual({ kind: "behind", release: "0.2.2", canUpgrade: true });
    expect(line({ version: "0.2.2" })).toBeNull();
    expect(line({ version: "0.2.3" })).toBeNull();
    expect(line({ status: "building" })).toBeNull();
    expect(line({ status: "offline", automatic: false })).toBeNull();
    expect(line({ status: "unknown" })).toBeNull();
    expect(line({ release: null })).toBeNull();
  });

  it("says an offline Server behind upgrades when it's back, while automatic upgrades are on", () => {
    expect(line({ status: "offline" })).toEqual({ kind: "when-back", release: "0.2.2" });
    expect(line({ status: "offline", version: "0.2.2" })).toBeNull();
  });

  it("offers neither Upgrade nor Try again while a rollout runs", () => {
    expect(line({ rolloutRunning: true })).toEqual({ kind: "behind", release: "0.2.2", canUpgrade: false });
    expect(line({ rolloutRunning: true, latest: attempt({ outcome: "failed", error: "boom" }) })).toMatchObject({ kind: "failed", canRetry: false });
  });

  it("offers nothing when the Server's version is unknown", () => {
    expect(line({ version: "" })).toBeNull();
  });

  it("reads a running attempt as upgrading", () => {
    expect(line({ latest: attempt() })).toEqual({ kind: "upgrading", target: "0.2.2" });
    expect(line({ latest: attempt({ targetVersion: null }) })).toEqual({ kind: "upgrading", target: "0.2.2" });
  });

  it("reads a requested Upgrade as upgrading until its attempt replaces the latest", () => {
    const earlier = attempt({ outcome: "failed", error: "boom" });
    expect(line({ latest: earlier, pendingFrom: earlier.attemptId })).toEqual({ kind: "upgrading", target: "0.2.2" });
    expect(line({ pendingFrom: null })).toEqual({ kind: "upgrading", target: "0.2.2" });
    expect(line({ latest: attempt({ attemptId: "b".repeat(32) }), pendingFrom: earlier.attemptId }))
      .toEqual({ kind: "upgrading", target: "0.2.2" });
  });

  it("says a failed Upgrade changed nothing only while the Server is online on its previous version", () => {
    const failed = attempt({ outcome: "failed", stage: "readiness", error: "readiness timed out; restored 0.2.1" });
    expect(line({ latest: failed })).toEqual({
      kind: "failed", target: "0.2.2", nothingChanged: true, details: "readiness timed out; restored 0.2.1", canRetry: true,
    });
    expect(line({ latest: failed, status: "offline" })).toMatchObject({ nothingChanged: false, canRetry: false });
    expect(line({ latest: failed, version: "0.2.0" })).toMatchObject({ nothingChanged: false });
    expect(line({ latest: failed, status: "building" })).toMatchObject({ nothingChanged: true, canRetry: false });
  });

  it("shows the stage reached for interrupted and unknown outcomes", () => {
    expect(line({ latest: attempt({ outcome: "interrupted", stage: "restarting" }) })).toMatchObject({ kind: "failed", details: "restarting" });
    expect(line({ latest: attempt({ outcome: "unknown", stage: "readiness" }) })).toMatchObject({ kind: "failed", details: "readiness" });
    expect(line({ latest: attempt({ outcome: "unknown", stage: null }) })).toMatchObject({ kind: "failed", details: null });
  });

  it("reads an attempt still running after the observation limit as unknown", () => {
    const stale = attempt({ stage: "restarting", startedAt: new Date(NOW - UPGRADE_OBSERVATION_LIMIT_MS).toISOString() });
    expect(line({ latest: stale })).toMatchObject({ kind: "failed", details: "restarting", canRetry: true });
  });

  it("drops a failure the Server has since moved past", () => {
    expect(line({ latest: attempt({ outcome: "failed", error: "boom" }), version: "0.2.2" })).toBeNull();
  });

  it("names the release when the failed request never resolved a target", () => {
    expect(line({ latest: attempt({ outcome: "failed", targetVersion: null, error: "no release" }) }))
      .toMatchObject({ kind: "failed", target: "0.2.2" });
  });

  it("is quiet after a success", () => {
    expect(line({ latest: attempt({ outcome: "succeeded" }), version: "0.2.2" })).toBeNull();
  });
});

describe("serversUpgradeLine", () => {
  const servers = (...versions: string[]) => versions.map((version, at) => ({ name: `web-${at + 1}`, version, status: "online" as const }));
  const servers4 = servers("0.2.2", "0.2.1", "0.2.1", "0.2.1");
  const summary = (input: Partial<Parameters<typeof serversUpgradeLine>[0]> = {}) => serversUpgradeLine({
    servers: servers4, release: "0.2.2", latest: [], lastUpgradedAt: null, pendingFrom: undefined, now: NOW, automatic: true, ...input,
  });

  it("says every Server is current, and when the latest successful attempt ran", () => {
    const at = new Date(NOW - 12 * 3_600_000).toISOString();
    expect(summary({ servers: servers("0.2.2", "0.2.2"), lastUpgradedAt: at })).toEqual({ kind: "current", release: "0.2.2", upgradedAt: at });
    expect(summary({ servers: servers("0.2.2", "0.2.3") })).toEqual({ kind: "current", release: "0.2.2", upgradedAt: null });
  });

  it("reads a running attempt as the rollout, counting every Server already on the release", () => {
    expect(summary({ latest: [attempt({ outcome: "succeeded" }), attempt({ attemptId: "b".repeat(32) })] }))
      .toEqual({ kind: "upgrading", target: "0.2.2", done: 1, total: 4 });
  });

  it("reads a requested rollout as upgrading until an attempt replaces the newest", () => {
    const earlier = attempt({ outcome: "failed", error: "boom", startedAt: new Date(NOW - 120_000).toISOString() });
    const newest = attempt({ attemptId: "c".repeat(32), outcome: "succeeded" });
    expect(summary({ pendingFrom: null })).toMatchObject({ kind: "upgrading", done: 1, total: 4 });
    expect(summary({ latest: [newest, earlier], pendingFrom: newest.attemptId })).toMatchObject({ kind: "upgrading" });
    expect(summary({ latest: [newest, earlier], pendingFrom: earlier.attemptId })).toMatchObject({ kind: "behind" });
  });

  it("stops reading a running attempt as the rollout after the observation limit", () => {
    const stale = attempt({ startedAt: new Date(NOW - UPGRADE_OBSERVATION_LIMIT_MS).toISOString() });
    expect(summary({ latest: [stale] })).toMatchObject({ kind: "behind" });
  });

  it("offers Upgrade with the version the Servers run while none has upgraded", () => {
    expect(summary({ servers: servers("0.2.1", "0.2.0", "") }))
      .toEqual({ kind: "behind", release: "0.2.2", upgraded: 0, total: 3, running: "0.2.0", canUpgrade: true });
    // Cloud can't upgrade a Server whose version is unknown, nor say it runs the release.
    expect(summary({ servers: servers("") })).toBeNull();
  });

  it("offers Upgrade the rest once some Servers have upgraded", () => {
    expect(summary()).toEqual({ kind: "behind", release: "0.2.2", upgraded: 1, total: 4, running: "0.2.1", canUpgrade: true });
  });

  it("names the offline Servers that upgrade when they're back, while automatic upgrades are on and only they are behind", () => {
    const offline = [
      { name: "web-1", version: "0.2.2", status: "online" as const },
      { name: "web-3", version: "0.2.1", status: "offline" as const },
    ];
    expect(summary({ servers: offline })).toEqual({ kind: "when-back", release: "0.2.2", names: ["web-3"] });
    expect(summary({ servers: offline, automatic: false })).toMatchObject({ kind: "behind", canUpgrade: false });
    expect(summary({ servers: [...offline, { name: "web-4", version: "0.2.1", status: "building" }] }))
      .toMatchObject({ kind: "behind", canUpgrade: false });
  });

  it("reads Servers ahead of the release, on a beta, as current", () => {
    expect(summary({ servers: servers("0.2.3-beta.1", "0.2.2") })).toMatchObject({ kind: "current", release: "0.2.2" });
    expect(summary({ servers: servers("0.2.3-beta.1", "0.2.3"), release: "0.2.3" })).toMatchObject({ kind: "behind", upgraded: 1, running: "0.2.3-beta.1" });
  });

  it("says nothing until the release is known, or with no Servers", () => {
    expect(summary({ release: null })).toBeNull();
    expect(summary({ servers: [] })).toBeNull();
  });
});

describe("newMajorLine", () => {
  it("names the unscoped stable release only when its major line is newer than the Servers'", () => {
    expect(newMajorLine("v0", "1.0.0")).toEqual({ release: "1.0.0", name: "1.0", running: "0.x" });
    expect(newMajorLine("v1", "3.2.0")).toEqual({ release: "3.2.0", name: "3.2", running: "1.x" });
    expect(newMajorLine("v0", "0.3.0")).toBeNull();
    expect(newMajorLine("v1", "0.9.0")).toBeNull();
    expect(newMajorLine(null, "1.0.0")).toBeNull();
    expect(newMajorLine("v0", null)).toBeNull();
  });
});
