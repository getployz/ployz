import type { ConfigCommand, DestructiveEffect } from "@ployz/sdk";
import { expect, it } from "vitest";
import { type ApprovalReview, approvalCommand, approvalSubject, approvalTarget, destroyedLine, destroyedLines, renamedAddresses } from "#/modules/approvals/approvals";
import { asksApproval } from "#/modules/config-store/store-asks";
import { approvalBadge } from "#/components/agent-sidebar/approval-copy";
import { asTestDouble } from "#/lib/test-double";

const here = { project: null, environment: null };
const id = "00000000-0000-4000-8000-0000000a5301";
const removeStaging: ConfigCommand = { command: "remove", project: "shop", environment: "staging" };
const shutDown: ConfigCommand = { command: "admit", admit: "shut_down", id, environment: here, version: null, accept_volume_loss: [] };
const cancel: ConfigCommand = { command: "cancel", deployment: id };
const retry: ConfigCommand = { command: "admit", admit: "retry", id, deployment: id, rolls_back: null, version: null, accept_volume_loss: [] };

it("asks for a removal or shutdown from `ployz` or an agent, never from the dashboard", () => {
  for (const via of ["agent", "cli"] as const) {
    expect(asksApproval(via, removeStaging)).toBe(true);
    expect(asksApproval(via, shutDown)).toBe(true);
  }
  expect(asksApproval("dashboard", removeStaging)).toBe(false);
  expect(asksApproval("dashboard", shutDown)).toBe(false);
});

it("sends a retry from `ployz` as one that may ask, since retrying a shutdown takes Services off", () => {
  expect(asksApproval("cli", retry)).toBe(true);
  expect(asksApproval("agent", retry)).toBe(true);
  expect(asksApproval("dashboard", retry)).toBe(false);
});

it("sends a cancel from `ployz` as one that may ask, and names it a Cancel", () => {
  expect(asksApproval("cli", cancel)).toBe(true);
  expect(asksApproval("agent", cancel)).toBe(true);
  expect(asksApproval("dashboard", cancel)).toBe(false);
  const review = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment: { name: "production", project: "shop" } } });
  expect(approvalCommand(cancel)).toBe("cancel");
  expect(approvalSubject(approvalCommand(cancel), review)).toMatchObject({ verb: "Cancel", name: "production" });
});

it("names a shutdown as a shutdown, not a Deploy", () => {
  const review = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment: { name: "staging", project: "shop" } } });
  expect(approvalCommand(shutDown)).toBe("shut_down");
  expect(approvalSubject(approvalCommand(shutDown), review)).toMatchObject({ verb: "Shut down", name: "staging" });
});

it("names a shutdown's retry a shutdown, so an approver never reads it as a redeploy", () => {
  const environment = { name: "production", project: "shop" };
  const removes = asTestDouble<ApprovalReview>()({ effects: [{ kind: "removes_service" }], diff: { environment } });
  expect(approvalSubject(approvalCommand(retry), removes)).toMatchObject({ verb: "Shut down", name: "production" });
  const redeploy = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment }, redeploy: { deployment: 4, rolls_back: [] } });
  expect(approvalSubject(approvalCommand(retry), redeploy)).toMatchObject({ verb: "Redeploy" });
  expect(approvalBadge(approvalCommand(retry), removes)).not.toBe(approvalBadge(approvalCommand(retry), redeploy));
});

it("lists a Store write's Environment under its Project, so two production approvals read apart", () => {
  const review = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment: { name: "production", project: "mcp-lab" } } });
  expect(approvalTarget("deploy", review)).toEqual({ verb: "Deploy", name: "mcp-lab/production" });
});

it("badges a Protected Volume Run by what it does to the Volume, never as a Deploy", () => {
  const run = (verb: string) => asTestDouble<ApprovalReview>()({ effects: [], operation: { verb, name: "pg-data", preview: {} }, protected: { environment: "production" } });
  const badges = (["move", "restore", "release", "resize"] as const).map((verb) => approvalBadge(verb, run(verb)));
  expect(new Set(badges).size).toBe(badges.length);
  for (const badge of badges) expect(badge).not.toMatch(/deploy/i);
  expect(approvalSubject("move", run("move"))).toMatchObject({ verb: "Move", name: "pg-data" });
});

it("names a Cluster rename by the private addresses it moves", () => {
  const rename = (from: string | null) => asTestDouble<ApprovalReview>()({ effects: [], operation: { verb: "set_cluster_name", name: "beta", preview: { from, to: "beta" } } });
  expect(approvalSubject("set_cluster_name", rename("acme"))).toMatchObject({ verb: "Rename cluster", name: "beta" });
  expect(renamedAddresses(rename("acme"))).toEqual({ from: "*.acme.ployz.internal", to: "*.beta.ployz.internal" });
  expect(renamedAddresses(rename(null))).toEqual({ from: null, to: "*.beta.ployz.internal" });
  expect(renamedAddresses(rename("beta"))).toEqual({ from: "*.beta.ployz.internal", to: "*.beta.ployz.internal" });
  expect(renamedAddresses(asTestDouble<ApprovalReview>()({ effects: [], operation: { verb: "move", name: "pg", preview: {} } }))).toBeNull();
});

const deletedVolume: DestructiveEffect = { kind: "deletes_volume", name: "pgdata", node: "v1", path: "volumes/v1" };

it("keeps real volume deletion wording for project removal, namespace clean and deploy", () => {
  for (const [subject, command] of [["project:shop", "remove"], ["namespace:shop", "clean"], ["server:box", "clean"], ["server:box", "remove"], ["server:box", "forget"], ["servers:acme", "forget"]] as const) {
    const review: ApprovalReview = { effects: [deletedVolume], operation: { subject, verb: command, name: "shop", preview: {} } };
    expect(approvalBadge(command, review)).toMatch(/deletes/i);
  }
  expect(destroyedLine(deletedVolume)).toMatchObject({ verb: expect.stringMatching(/^Deletes/), name: "pgdata", after: expect.stringMatching(/not recoverable/i) });
  expect(destroyedLine(deletedVolume, "publish").verb).toMatch(/^Next deploy/);
});

it("lists a pruning Deploy's removal of other services, with the Servers it couldn't check, as the card does (#3511)", () => {
  const environment = { name: "production", project: "verify" };
  const prune = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment }, prunes: true, dropped: ["machine-3"] });
  const pruned = destroyedLines("admit", prune);
  expect(pruned).toHaveLength(1);
  expect(pruned[0]).toContain("machine-3");
  const checked = asTestDouble<ApprovalReview>()({ effects: [{ kind: "removes_service", name: "worker" }], diff: { environment }, prunes: true });
  const lines = destroyedLines("admit", checked);
  expect(lines).toHaveLength(2);
  expect(lines[0]).toContain("worker");
  expect(lines[1]).not.toContain("machine-3");
  const plain = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment } });
  expect(destroyedLines("admit", plain)).toEqual([]);
});
