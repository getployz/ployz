import { describe, expect, it } from "vitest";
import { type ClusterDomainRow, clusterDomainStatus } from "#/modules/cluster-domain/cluster-domain";

const now = new Date("2026-09-25T12:00:00Z");
const ready: Pick<ClusterDomainRow, "recordsSyncedAt" | "traffic" | "certificateNotAfter"> = {
  recordsSyncedAt: new Date("2026-09-25T11:00:00Z"),
  traffic: { kind: "probed", reachable: [], unreachable: [] },
  certificateNotAfter: new Date("2026-12-25T00:00:00Z"),
};
const blocked = [{ machineId: "a", address: "203.0.113.1" }, { machineId: "b", address: "2001:db8::1" }] as
  Extract<NonNullable<ClusterDomainRow["traffic"]>, { kind: "probed" }>["unreachable"];
const serving = [{ machineId: "c", address: "198.51.100.7" }] as typeof blocked;
const none = [] as const;

describe("clusterDomainStatus", () => {
  it.each([
    ["healthy", ready, { kind: "ready" }],
    ["never checked, or the Cluster offline", { ...ready, traffic: null }, { kind: "ready" }],
    ["records not yet published", { ...ready, recordsSyncedAt: null }, { kind: "setting_up" }],
    ["no wildcard yet", { ...ready, certificateNotAfter: null }, { kind: "setting_up" }],
    ["no Servers", { ...ready, traffic: { kind: "no_servers" } }, { kind: "attention", reason: "no_servers" }],
    ["no public IP", { ...ready, traffic: { kind: "no_public_ip" } }, { kind: "attention", reason: "no_public_ip" }],
    ["port 80 blocked", { ...ready, traffic: { kind: "probed", reachable: blocked.slice(0, 0), unreachable: blocked } },
      { kind: "attention", reason: "port_80", addresses: ["203.0.113.1", "2001:db8::1"] }],
    ["something else on port 80", { ...ready, traffic: { kind: "probed", reachable: blocked.slice(0, 0), unreachable: blocked.slice(0, 0), taken: blocked } },
      { kind: "attention", reason: "port_80_taken", addresses: ["203.0.113.1", "2001:db8::1"] }],
    ["port 80 closed on one Server, taken on another",
      { ...ready, traffic: { kind: "probed", reachable: blocked.slice(0, 0), unreachable: blocked.slice(0, 1), taken: blocked.slice(1) } },
      { kind: "attention", reason: "port_80", addresses: ["203.0.113.1"] }],
    ["wildcard expired", { ...ready, certificateNotAfter: new Date("2026-09-24T00:00:00Z") },
      { kind: "attention", reason: "https_down" }],
    // What the user can fix comes before what Ployz is fixing or still setting up.
    ["no Servers and nothing published", { ...ready, traffic: { kind: "no_servers" }, recordsSyncedAt: null },
      { kind: "attention", reason: "no_servers" }],
    ["port 80 blocked and wildcard expired",
      { ...ready, traffic: { kind: "probed", reachable: blocked.slice(0, 0), unreachable: blocked }, certificateNotAfter: new Date("2026-09-24T00:00:00Z") },
      { kind: "attention", reason: "port_80", addresses: ["203.0.113.1", "2001:db8::1"] }],
    // Checked before the sync stored which Servers answered: whether any did is unknown.
    ["port 80 blocked, checked before reachable was stored", { ...ready, traffic: { kind: "probed", unreachable: blocked } },
      { kind: "attention", reason: "port_80", addresses: ["203.0.113.1", "2001:db8::1"] }],
  ] as const)("%s", (_, domain, state) => {
    expect(clusterDomainStatus(domain, now)).toEqual({ state, excluded: none });
  });

  it("serves traffic through the Servers that answer, and names the ones left out", () => {
    const domain = { ...ready, traffic: { kind: "probed", reachable: serving, unreachable: blocked.slice(0, 1), taken: blocked.slice(1) } } as const;

    expect(clusterDomainStatus(domain, now)).toEqual({
      state: { kind: "ready" },
      excluded: [{ address: "203.0.113.1", reason: "port_80" }, { address: "2001:db8::1", reason: "port_80_taken" }],
    });
  });

  it("keeps a left-out Server separate from what Ployz is still setting up", () => {
    const domain = { ...ready, recordsSyncedAt: null, traffic: { kind: "probed", reachable: serving, unreachable: blocked.slice(0, 1) } } as const;

    expect(clusterDomainStatus(domain, now)).toEqual({
      state: { kind: "setting_up" },
      excluded: [{ address: "203.0.113.1", reason: "port_80" }],
    });
  });
});
