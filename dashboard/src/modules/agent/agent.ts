import type { ConfigWritten } from "@ployz/sdk";
import { defineInterrupt } from "@tanstack/ai/client";
import { Schema } from "effect";
import type { StoreRefusal } from "#/modules/config-store/store.contract.ts";

/**
 * A gated Publish or Deploy waiting on a human, keyed by its tool call id. The payload names the pending Approval;
 * the sidebar draws the card from that row, never from the payload, and the server re-reads the row on resume
 * rather than trusting the client's empty response.
 */
export const approvalInterrupt = defineInterrupt({
  id: "ployz.approval",
  payloadSchema: Schema.toStandardJSONSchemaV1(Schema.Struct({ approvalId: Schema.String })),
  responseSchema: Schema.toStandardJSONSchemaV1(Schema.Struct({})),
});

/** What every sidebar tool hands the model: the Store's answer or its refusal, as `ployz --json` prints them. */
export type ToolOutcome<T = unknown> = { ok: true; value: T } | { ok: false; refusal: StoreRefusal } | { ok: false; cancelled: true };

export type DeployOutcome = ToolOutcome<Extract<ConfigWritten, { written: "deployment" }>>;

