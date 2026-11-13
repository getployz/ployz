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
  it("retries the refused Save in its own Project and Environment at the version the Store named", () => {
    const history = exchange("deploy", { skip_deploy: true, project: "shop", environment: "staging" }, loss("9:1:0.1:abc"));
    expect(asking("save, accepting the volume loss", history)).toEqual({ calls: [{ tool: "deploy", input: {
      skip_deploy: true,
      project: "shop", environment: "staging", accept_volume_loss: ["data"], version: "9:1:0.1:abc",
    } }] });
  });

  it("retries a Deploy at the version the Store named", () => {
    const history = exchange("deploy", {}, loss("4:2:0.1:def"));
    expect(asking("deploy, accepting the volume loss", history)).toEqual({ calls: [{ tool: "deploy", input: {
      accept_volume_loss: ["data"], version: "4:2:0.1:def",
    } }] });
  });

  it("accepts the loss of the action asked for when another action refused later", () => {
    const history = [
      ...exchange("deploy", { skip_deploy: true }, loss("9:1:0.1:save")),
      ...exchange("deploy", {}, loss("9:1:0.1:deploy")),
    ];
    expect(asking("save, accepting the volume loss", history)).toMatchObject({ calls: [{ tool: "deploy", input: { skip_deploy: true, version: "9:1:0.1:save" } }] });
    expect(asking("deploy, accepting the volume loss", history)).toMatchObject({ calls: [{ tool: "deploy", input: { version: "9:1:0.1:deploy" } }] });
  });

  it("keeps each Environment's refusal to that Environment", () => {
    const history = [
      ...exchange("deploy", { skip_deploy: true, environment: "staging" }, loss("1:1:0.1:staging")),
      ...exchange("deploy", { skip_deploy: true, environment: "production" }, loss("2:1:0.1:production")),
    ];
    expect(asking("save --environment staging, accepting the volume loss", history))
      .toMatchObject({ calls: [{ tool: "deploy", input: { skip_deploy: true, environment: "staging", version: "1:1:0.1:staging" } }] });
    expect(asking("save, accepting the volume loss", history))
      .toMatchObject({ calls: [{ tool: "deploy", input: { skip_deploy: true, environment: "production", version: "2:1:0.1:production" } }] });
    expect(asking("save --environment preview, accepting the volume loss", history)).toHaveProperty("text");
  });

  it("revives no refusal past a denial or a cancellation, or one a later result of that action answered", () => {
    const refused = exchange("deploy", { skip_deploy: true }, loss("9:1:0.1:abc"));
    const denied = exchange("deploy", {}, { ok: false, refusal: { code: "approval_denied", message: "A human denied this deploy.", details: null } });
    const cancelled = exchange("deploy", {}, { ok: false, cancelled: true });
    const saved = exchange("deploy", { skip_deploy: true }, { ok: true, value: { saved: { version: "9:1:0.1:abc", created: true } } });
    for (const later of [denied, cancelled, saved]) {
      expect(asking("save, accepting the volume loss", [...refused, ...later])).toHaveProperty("text");
    }
  });

  it("accepts nothing when the refusal names no loss or a name it doesn't list", () => {
    for (const outcome of [loss("9:1:0.1:abc", []), loss("9:1:0.1:abc", ["cache"]), { ok: false, refusal: { code: "conflict", message: "stale", details: null } }]) {
      expect(asking("save, accepting the volume loss", exchange("deploy", { skip_deploy: true }, outcome))).toHaveProperty("text");
    }
  });

  it("saves at exactly the version the member named, empty included", () => {
    expect(stubScript([{ role: "user", content: 'save --version "9:1:0.1:AbC"' }])).toEqual({ calls: [{ tool: "deploy", input: { skip_deploy: true, version: "9:1:0.1:AbC" } }] });
    expect(stubScript([{ role: "user", content: 'save --version ""' }])).toEqual({ calls: [{ tool: "deploy", input: { skip_deploy: true, version: "" } }] });
  });
});

describe("the scripted model proposing", () => {
  const id = "00000000-0000-4000-8000-000000000001";
  const said = "propose stop web, restart api, start web";

  it("opens a proposal, stages each step with proposal set, then presents it", () => {
    expect(asking(said, [])).toEqual({ calls: [{ tool: "proposal_create", input: {} }] });
    const opened = exchange("proposal_create", {}, { ok: true, value: { proposal_id: id } });
    const history = [{ role: "user" as const, content: said }, ...opened];
    expect(stubScript(history)).toEqual({ calls: [{ tool: "service_stop", input: { service: "web", proposal: id } }] });
    const one = [...history, ...exchange("service_stop", { service: "web", proposal: id }, { ok: true, value: { step: 1 } })];
    expect(stubScript(one)).toEqual({ calls: [{ tool: "service_restart", input: { service: "api", proposal: id } }] });
    const two = [...one, ...exchange("service_restart", { service: "api", proposal: id }, { ok: true, value: { step: 2 } })];
    expect(stubScript(two)).toEqual({ calls: [{ tool: "service_start", input: { service: "web", proposal: id } }] });
    const three = [...two, ...exchange("service_start", { service: "web", proposal: id }, { ok: true, value: { step: 3 } })];
    expect(stubScript(three)).toEqual({ calls: [{ tool: "proposal_present", input: { proposal: id } }] });
    const presented = [...three, ...exchange("proposal_present", { proposal: id }, { ok: true, value: { steps: 3 } })];
    expect(stubScript(presented)).toEqual({ text: `Presented proposal ${id}. Say "follow ${id}" to see how far it got.` });
  });

  it("quotes a refusal and stages nothing more", () => {
    const opened = exchange("proposal_create", {}, { ok: true, value: { proposal_id: id } });
    const refused = exchange("service_stop", { service: "web", proposal: id }, { ok: false, refusal: { code: "not_found", message: "No service is named web in shop/production." } });
    expect(stubScript([{ role: "user", content: said }, ...opened, ...refused])).toEqual({ text: "The proposal stopped: No service is named web in shop/production." });
  });

  it("reads where a proposal stands and says where each step got", () => {
    expect(asking(`follow ${id}`, [])).toEqual({ calls: [{ tool: "proposal_status", input: { proposal: id } }] });
    const stood = exchange("proposal_status", { proposal: id }, {
      ok: true,
      value: {
        status: "approved",
        steps: [
          { index: 1, verb: "stop", service: "web", state: "done", reason: null },
          { index: 2, verb: "restart", service: "api", state: "failed", reason: "machine-2 didn't accept the approval." },
          { index: 3, verb: "start", service: "web", state: "not_run", reason: null },
        ],
      },
    });
    expect(stubScript([{ role: "user", content: `follow ${id}` }, ...stood])).toEqual({
      text: "Approved. 1. stop web: done. 2. restart api: failed, machine-2 didn't accept the approval. 3. start web: not run.",
    });
  });
});
