import type { MachineId, OperationId } from "@ployz/sdk";
import type { VolumeApproval, VolumeWorkflow } from "@ployz/sdk/authority";
import { describe, expect, it } from "vitest";
import { canonicalJson } from "./approval-session";

const a = "a".repeat(32) as MachineId;
const b = "b".repeat(32) as MachineId;
const c = "c".repeat(32) as MachineId;
const d = "d".repeat(64);

const head = `{"cluster":"CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=","expires_at":1000000,"issued_at":100000,"namespace":"app","operation":"0f8fad5bd9cb469fa16570867728950e","signer":"ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c","volume":"app_data","workflow":`;

// The bodies `ployz_core::authority::volume`'s golden test pins, one per workflow.
const workflows: [VolumeWorkflow, string][] = [
  [
    { kind: "move", from: a, to: b, service: { name: "db", digest: d }, mirrors: [c] },
    `{"from":"${a}","kind":"move","mirrors":["${c}"],"service":{"digest":"${d}","name":"db"},"to":"${b}"}`,
  ],
  [{ kind: "mirror", writer: a, mirror: b, replace: false }, `{"kind":"mirror","mirror":"${b}","replace":false,"writer":"${a}"}`],
  [
    { kind: "mirror_removal", mirror: b, writer: a, role: "mirror" },
    `{"kind":"mirror_removal","mirror":"${b}","role":"mirror","writer":"${a}"}`,
  ],
  [
    { kind: "restore", machine: b, at: "18446744073709551615", loses_after: 500000 },
    `{"at":"18446744073709551615","kind":"restore","loses_after":500000,"machine":"${b}"}`,
  ],
  [
    { kind: "resize", machines: [a, b], maximum_bytes: 20_000_000_000, shrink: false },
    `{"kind":"resize","machines":["${a}","${b}"],"maximum_bytes":20000000000,"shrink":false}`,
  ],
  [{ kind: "seed", machine: a, source: "app_old", at: "42" }, `{"at":"42","kind":"seed","machine":"${a}","source":"app_old"}`],
];

describe("a Volume approval's canonical body", () => {
  it.each(workflows)("matches ployz-core's bytes for %j", (workflow, golden) => {
    const approval = {
      cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
      operation: "0f8fad5bd9cb469fa16570867728950e" as OperationId,
      namespace: "app",
      volume: "app_data",
      workflow,
      issued_at: 100000,
      expires_at: 1000000,
      signer: "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
    } satisfies VolumeApproval;
    expect(canonicalJson(approval)).toBe(`${head}${golden}}`);
  });
});
