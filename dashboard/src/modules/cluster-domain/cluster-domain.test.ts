import { describe, expect, it } from "vitest";
import { type ClusterDomainRow, clusterDomainStatus } from "#/modules/cluster-domain/cluster-domain";

const now = new Date("2026-09-25T12:00:00Z");
const ready: Pick<ClusterDomainRow, "recordsSyncedAt" | "traffic" | "certificateNotAfter"> = {
  recordsSyncedAt: new Date("2026-09-25T11:00:00Z"),
  traffic: { kind: "probed", unreachable: [] },
  certificateNotAfter: new Date("2026-12-25T00:00:00Z"),
};
const blocked = [{ machineId: "a", address: "203.0.113.1" }, { machineId: "b", address: "2001:db8::1" }] as
  Extract<NonNullable<ClusterDomainRow["traffic"]>, { kind: "probed" }>["unreachable"];

describe("clusterDomainStatus", () => {
  it.each([
    ["healthy", ready, { kind: "ready" }],
    ["never checked, or the Cluster offline", { ...ready, traffic: null }, { kind: "ready" }],
    ["records not yet published", { ...ready, recordsSyncedAt: null }, { kind: "setting_up" }],
    ["no wildcard yet", { ...ready, certificateNotAfter: null }, { kind: "setting_up" }],
    ["no Servers", { ...ready, traffic: { kind: "no_servers" } }, { kind: "attention", reason: "no_servers" }],
    ["no public IP", { ...ready, traffic: { kind: "no_public_ip" } }, { kind: "attention", reason: "no_public_ip" }],
    ["port 80 blocked", { ...ready, traffic: { kind: "probed", unreachable: blocked } },
      { kind: "attention", reason: "port_80", addresses: ["203.0.113.1", "2001:db8::1"] }],
    ["wildcard expired", { ...ready, certificateNotAfter: new Date("2026-09-24T00:00:00Z") },
      { kind: "attention", reason: "https_down" }],
    // What the user can fix comes before what Ployz is fixing or still setting up.
    ["no Servers and nothing published", { ...ready, traffic: { kind: "no_servers" }, recordsSyncedAt: null },
      { kind: "attention", reason: "no_servers" }],
    ["port 80 blocked and wildcard expired",
      { ...ready, traffic: { kind: "probed", unreachable: blocked }, certificateNotAfter: new Date("2026-09-24T00:00:00Z") },
      { kind: "attention", reason: "port_80", addresses: ["203.0.113.1", "2001:db8::1"] }],
  ] as const)("%s", (_, domain, expected) => {
    expect(clusterDomainStatus(domain, now)).toEqual(expected);
  });
});
