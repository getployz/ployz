import type { ConfigCommand } from "@ployz/sdk";
import { expect, it } from "vitest";
import { type ApprovalReview, approvalCommand, approvalSubject, asksApproval } from "#/modules/approvals/approvals";
import { asTestDouble } from "#/lib/test-double";

const here = { project: null, environment: null };
const id = "00000000-0000-4000-8000-0000000a5301";
const removeStaging: ConfigCommand = { command: "remove", project: "shop", environment: "staging" };
const shutDown: ConfigCommand = { command: "admit", admit: "shut_down", id, environment: here, version: null, accept_volume_loss: [] };

it("asks for an agent's removal or shutdown, never a person's", () => {
  expect(asksApproval("agent", removeStaging)).toBe(true);
  expect(asksApproval("agent", shutDown)).toBe(true);
  expect(asksApproval("cli", removeStaging)).toBe(false);
  expect(asksApproval("dashboard", shutDown)).toBe(false);
});

it("names a shutdown as a shutdown, not a Deploy", () => {
  const review = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment: { name: "staging", project: "shop" } } });
  expect(approvalCommand(shutDown)).toBe("shut_down");
  expect(approvalSubject(approvalCommand(shutDown), review)).toMatchObject({ verb: "Shut down", name: "staging" });
});
