import type { ConfigWritten } from "@ployz/sdk";
import { defineInterrupt } from "@tanstack/ai/client";
import { Schema } from "effect";
import type { StoreRefusal } from "#/modules/config-store/store.contract.ts";

type JsonObjectSchema = { type: "object"; properties: Record<string, { type: "string" }>; required?: string[]; additionalProperties: false };

/**
 * A Standard Schema with its JSON Schema written out, as interrupts need. Written by hand because Effect's JSON Schema
 * encoder lands in the chunk every page loads.
 */
const portable = <S extends Schema.ConstraintDecoder<object>>(schema: S, json: JsonObjectSchema) => {
  const standard = Schema.toStandardSchemaV1(schema)["~standard"];
  return { "~standard": { ...standard, jsonSchema: { input: () => json, output: () => json } } };
};

/**
 * A gated Publish or Deploy waiting on a human, keyed by its tool call id. The payload names the pending Approval;
 * the sidebar draws the card from that row, never from the payload, and the server re-reads the row on resume
 * rather than trusting the client's empty response.
 */
export const approvalInterrupt = defineInterrupt({
  id: "ployz.approval",
  payloadSchema: portable(Schema.Struct({ approvalId: Schema.String }), {
    type: "object",
    properties: { approvalId: { type: "string" } },
    required: ["approvalId"],
    additionalProperties: false,
  }),
  responseSchema: portable(Schema.Struct({}), { type: "object", properties: {}, additionalProperties: false }),
});

/**
 * What every sidebar tool hands the model: the Store's answer or its refusal, as `ployz --json` prints them. A Publish or
 * Deploy the Store reviewed for an Organization that asks, and let through without asking, says it destroyed nothing.
 */
export type ToolOutcome<T = unknown> =
  | { ok: true; value: T; nothing_destroyed?: true }
  | { ok: false; refusal: StoreRefusal }
  | { ok: false; cancelled: true };

export type DeployOutcome = ToolOutcome<Extract<ConfigWritten, { written: "deployment" }>>;

