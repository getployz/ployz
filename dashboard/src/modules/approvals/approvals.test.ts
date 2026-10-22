import type { ConfigCommand } from "@ployz/sdk";
import { expect, it } from "vitest";
import { type ApprovalReview, approvalBadge, approvalCommand, approvalSubject, approvalTarget, asksApproval } from "#/modules/approvals/approvals";
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
  expect(approvalBadge(approvalCommand(cancel), review)).toBe("Cancels deploy");
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
  expect(approvalBadge(approvalCommand(retry), removes)).toBe("Destroys 1 thing");
  const redeploy = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment }, redeploy: { deployment: 4, rolls_back: [] } });
  expect(approvalSubject(approvalCommand(retry), redeploy)).toMatchObject({ verb: "Redeploy" });
  expect(approvalBadge(approvalCommand(retry), redeploy)).toBe("Redeploys");
});

it("lists a Store write's Environment under its Project, so two production approvals read apart", () => {
  const review = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment: { name: "production", project: "mcp-lab" } } });
  expect(approvalTarget("deploy", review)).toEqual({ verb: "Deploy", name: "mcp-lab/production" });
});
