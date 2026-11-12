import type { MachineId, OperationId } from "@ployz/sdk";
import type { VolumeApproval, VolumeWorkflow } from "@ployz/sdk/authority";
import { volumeApprovalBody } from "@ployz/sdk/authority";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Member } from "#/modules/volume-run/volume-run";
import { COPIES } from "#/modules/volume-run/volume-run.test-fixture";
import * as prompt from "./approval-prompt";
import { canonicalJson } from "./approval-session";
import * as passkey from "./passkey-statement";
import { approveVolume, type VolumeChange, VolumeNotApproved, type VolumeScope } from "./volume-approval";

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

describe("the browser's draft", () => {
  // The input `ployz_core::authority::volume_draft`'s browser golden test hands the drafter, and the body it pins.
  it("drafts the bytes ployz-core pins for the browser", () => {
    const draft = volumeApprovalBody({
      cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
      operation: "0f8fad5bd9cb469fa16570867728950e" as OperationId,
      namespace: "app",
      volume: "app_data",
      name: "data",
      servers: { [a]: "web-1", [b]: "web-2" },
      workflow: { kind: "resize", machines: [b, a], maximum_bytes: 20_000_000_000, shrink: false },
      issued_at: 100_000,
      signer: "passkey",
    });
    expect(draft.body).toBe(
      `{"cluster":"CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=","expires_at":1000000,"issued_at":100000,"namespace":"app","operation":"0f8fad5bd9cb469fa16570867728950e","signer":"passkey","volume":"app_data","workflow":{"kind":"resize","machines":["${a}","${b}"],"maximum_bytes":20000000000,"shrink":false}}`,
    );
    expect(draft.lines[0]).toBe("Approved: set data to 20GB on web-1, web-2. No copy shrinks.");
  });

  it("shows a relayed name that could pass for another quoted, whitespace and all", () => {
    const draft = volumeApprovalBody({
      cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
      operation: "0f8fad5bd9cb469fa16570867728950e" as OperationId,
      namespace: "app",
      volume: "app_data",
      name: "data",
      servers: { [a]: "web-1", [b]: "web 2\n" },
      workflow: { kind: "mirror", writer: a, mirror: b, replace: false },
      issued_at: 100_000,
      signer: "passkey",
    });
    expect(draft.lines).toContain(`  mirror "web 2\\n" (${b})`);
  });
});

describe("approveVolume", () => {
  const holding = (id: MachineId, name: string, enforces: boolean, passkey: boolean): Member =>
    ({ machine: { id, name }, address: "fdcc::1", answered: true, view: { copy: COPIES.writer }, enforces, passkey });
  const scope = (members: Member[]): VolumeScope =>
    ({ cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=", namespace: "app", dockerVolume: "app_data", name: "data", members });
  const resize: VolumeChange = {
    title: "Grow data", action: "Approve and grow", parties: [a, b], command: "ployz volume set data --size 20",
    workflow: { kind: "resize", machines: [a, b], maximum_bytes: 20_000_000_000, shrink: false },
  };
  const ASSERTION = { credential_id: "AQID", authenticator_data: "AQID", client_data_json: "AQID", signature: "AQID" };

  afterEach(() => vi.restoreAllMocks());

  it("asks nothing when no Server it names enforces", async () => {
    const review = vi.spyOn(prompt, "reviewApproval");
    expect(await approveVolume(scope([holding(a, "web-1", false, true), holding(b, "web-2", false, false)]), resize)).toBeUndefined();
    expect(review).not.toHaveBeenCalled();
  });

  it("refuses before any prompt when an enforcing Server can't take a passkey's answer", async () => {
    const review = vi.spyOn(prompt, "reviewApproval");
    await expect(approveVolume(scope([holding(a, "web-1", true, true), holding(b, "web-2", true, false)]), resize))
      .rejects.toEqual(new VolumeNotApproved("web-2 needs a newer Ployz to approve this from the dashboard. Upgrade it, or run ployz volume set data --size 20 from an approved device."));
    expect(review).not.toHaveBeenCalled();
  });

  it("shows every line it signs, then asks the passkey once for exactly those bytes", async () => {
    const review = vi.spyOn(prompt, "reviewApproval").mockResolvedValue(true);
    const sign = vi.spyOn(passkey, "signWithPasskey").mockImplementation((body) => Promise.resolve({ body, signatures: [], assertions: [ASSERTION] }));
    const approved = await approveVolume(scope([holding(a, "web-1", true, true), holding(b, "web-2", false, false)]), resize, 100_000);
    expect(sign).toHaveBeenCalledOnce();
    const [body, purpose] = sign.mock.calls[0] ?? [];
    expect(purpose).toBe("volume");
    expect(JSON.parse(body ?? "")).toMatchObject({ signer: "passkey", volume: "app_data", namespace: "app", issued_at: 100_000, expires_at: 1_000_000 });
    expect(review.mock.calls[0]?.[0].lines).toEqual(approved?.lines);
    expect(approved?.lines[0]).toBe("Approved: set data to 20GB on web-1, web-2. No copy shrinks.");
    expect(approved?.approval).toEqual({ body, signatures: [], assertions: [ASSERTION] });
  });

  it("signs nothing when the person cancels", async () => {
    vi.spyOn(prompt, "reviewApproval").mockResolvedValue(false);
    const sign = vi.spyOn(passkey, "signWithPasskey");
    expect(await approveVolume(scope([holding(a, "web-1", true, true), holding(b, "web-2", true, true)]), resize)).toBeNull();
    expect(sign).not.toHaveBeenCalled();
  });

  it("says how to add a passkey when none answers", async () => {
    vi.spyOn(prompt, "reviewApproval").mockResolvedValue(true);
    vi.spyOn(passkey, "signWithPasskey").mockRejectedValue(Object.assign(new Error("denied"), { name: "NotAllowedError" }));
    await expect(approveVolume(scope([holding(a, "web-1", true, true), holding(b, "web-2", true, true)]), resize))
      .rejects.toThrow("add a passkey under Account");
  });
});
