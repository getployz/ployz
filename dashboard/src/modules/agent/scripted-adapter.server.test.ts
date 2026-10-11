import type { ModelMessage } from "@tanstack/ai";
import { describe, expect, it } from "vitest";
import type { JsonObject } from "#/db/tables";
import { stubScript } from "./scripted-adapter.server";

let calls = 0;

const exchange = (tool: string, input: JsonObject, outcome: JsonObject): ModelMessage[] => {
  const id = `call-${++calls}`;
  return [
    { role: "assistant", content: null, toolCalls: [{ id, type: "function", function: { name: tool, arguments: JSON.stringify(input) } }] },
    { role: "tool", content: JSON.stringify(outcome), toolCallId: id },
  ];
};

const loss = (version: string, accept = ["data"]) => ({
  ok: false,
  refusal: {
    code: "confirmation_required",
    message: "This removes Volume data.",
    details: { volumes: [{ name: "data", deletes: [{ machine_id: "m1" }] }], accept, version },
  },
});

const asking = (said: string, history: ModelMessage[]) => stubScript([...history, { role: "user", content: said }]);

describe("the scripted model accepting Volume loss", () => {
  it("retries the refused Publish in its own Project and Environment at the version the Store named", () => {
    const history = exchange("publish", { project: "shop", env: "staging" }, loss("9:1:0.1:abc"));
    expect(asking("publish, accepting the volume loss", history)).toEqual({ calls: [{ tool: "publish", input: {
      project: "shop", env: "staging", accept_volume_loss: ["data"], version: "9:1:0.1:abc",
    } }] });
  });

  it("retries a Deploy with expect_version, not version", () => {
    const history = exchange("deploy", {}, loss("4:2:0.1:def"));
    expect(asking("deploy, accepting the volume loss", history)).toEqual({ calls: [{ tool: "deploy", input: {
      accept_volume_loss: ["data"], expect_version: "4:2:0.1:def",
    } }] });
  });

  it("accepts the loss of the action asked for when another action refused later", () => {
    const history = [
      ...exchange("publish", {}, loss("9:1:0.1:publish")),
      ...exchange("deploy", {}, loss("9:1:0.1:deploy")),
    ];
    expect(asking("publish, accepting the volume loss", history)).toMatchObject({ calls: [{ tool: "publish", input: { version: "9:1:0.1:publish" } }] });
    expect(asking("deploy, accepting the volume loss", history)).toMatchObject({ calls: [{ tool: "deploy", input: { expect_version: "9:1:0.1:deploy" } }] });
  });

  it("keeps each Environment's refusal to that Environment", () => {
    const history = [
      ...exchange("publish", { env: "staging" }, loss("1:1:0.1:staging")),
      ...exchange("publish", { env: "production" }, loss("2:1:0.1:production")),
    ];
    expect(asking("publish --env staging, accepting the volume loss", history))
      .toMatchObject({ calls: [{ tool: "publish", input: { env: "staging", version: "1:1:0.1:staging" } }] });
    expect(asking("publish, accepting the volume loss", history))
      .toMatchObject({ calls: [{ tool: "publish", input: { env: "production", version: "2:1:0.1:production" } }] });
    expect(asking("publish --env preview, accepting the volume loss", history)).toHaveProperty("text");
  });

  it("revives no refusal past a denial or a cancellation, or one a later result of that action answered", () => {
    const refused = exchange("publish", {}, loss("9:1:0.1:abc"));
    const denied = exchange("deploy", {}, { ok: false, refusal: { code: "approval_denied", message: "A human denied this deploy.", details: null } });
    const cancelled = exchange("deploy", {}, { ok: false, cancelled: true });
    const published = exchange("publish", {}, { ok: true, value: { written: "published" } });
    for (const later of [denied, cancelled, published]) {
      expect(asking("publish, accepting the volume loss", [...refused, ...later])).toHaveProperty("text");
    }
  });

  it("accepts nothing when the refusal names no loss or a name it doesn't list", () => {
    for (const outcome of [loss("9:1:0.1:abc", []), loss("9:1:0.1:abc", ["cache"]), { ok: false, refusal: { code: "conflict", message: "stale", details: null } }]) {
      expect(asking("publish, accepting the volume loss", exchange("publish", {}, outcome))).toHaveProperty("text");
    }
  });

  it("publishes at exactly the version the member named, empty included", () => {
    expect(stubScript([{ role: "user", content: 'publish --version "9:1:0.1:AbC"' }])).toEqual({ calls: [{ tool: "publish", input: { version: "9:1:0.1:AbC" } }] });
    expect(stubScript([{ role: "user", content: 'publish --version ""' }])).toEqual({ calls: [{ tool: "publish", input: { version: "" } }] });
  });
});
