import type { ConfigCommand } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asksApproval } from "#/modules/approvals/approvals";

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
