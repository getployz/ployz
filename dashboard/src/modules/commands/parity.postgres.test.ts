import { it } from "@effect/vitest";
import type { JsonValue } from "@ployz/sdk";
import { EventType, type StreamChunk } from "@tanstack/ai";
import { fakeText } from "@tanstack/ai/testing";
import { asc, eq } from "drizzle-orm";
import { Cause, ConfigProvider, Effect, Exit, Layer, Schema } from "effect";
import { expect } from "vitest";
import { asRecord } from "#/lib/json";
import { asTestDouble } from "#/lib/test-double";
import { agentChat } from "#/modules/agent/agent-chat.server";
import { operationApprovals } from "#/modules/approvals/tables";
import { manifest } from "#/modules/commands/commands.server";
import { COMMANDS } from "#/modules/commands/registry.server";
import type { Caller } from "#/modules/identity/actor";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { organizationServerUpgrades } from "#/modules/server-upgrade/tables";
import { handleRunRequest } from "#/routes/api/cli/-handlers";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { encodePublicError, statusForPublicError } from "#/server/public-error";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c301";

/** A tool result as the model reads it: a command's outcome, or the agent's word that no such tool exists. */
const Heard = Schema.fromJsonString(Schema.Union([
  Schema.Struct({ ok: Schema.Boolean, value: Schema.optional(Schema.Unknown), refusal: Schema.optional(Schema.Struct({ code: Schema.String, message: Schema.String })) }),
  Schema.Struct({ error: Schema.String }),
]));

/**
 * Shop with two pending approvals, Ada in her sidebar, and an Organization Token of hers for the route. `route` and
 * `sidebar` call one command the way `ployz` and the agent do; `rows` is everything a published command can change.
 */
const parity = Effect.fn(function* () {
  const cloud = yield* storeTestCloud();
  const stub = ConfigProvider.layer(ConfigProvider.fromEnv({ env: { PLOYZ_AGENT_STUB: "1" } }));
  const services = yield* Layer.build(Layer.mergeAll(AuthLive.pipe(Layer.provide(cloud)), cloud, stub));
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, support: null };
  const { secret } = yield* provided(createOrganizationToken(caller, { name: "parity", expiresInDays: 1 }));
  const approvals = yield* provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.insert(operationApprovals).values(["volume:data", "volume:logs"].map((subject) => ({
      organizationId: ORGANIZATION, subject, credentialKind: "session" as const, credentialId: "device", command: "release", digest: `release:${subject}`,
      review: asTestDouble<typeof operationApprovals.$inferInsert.review>()({
        effects: [{ kind: "deletes_volume", name: subject.slice("volume:".length) }],
        operation: { verb: "release", name: subject.slice("volume:".length), preview: null },
      }),
    }))).returning({ id: operationApprovals.id });
  }));

  const route = (name: string, input: JsonValue) => Effect.gen(function* () {
    const request = new Request(`http://localhost:3000/api/cli/run/${name}`, {
      method: "POST",
      headers: { authorization: `Bearer ${secret}`, "content-type": "application/json" },
      body: JSON.stringify({ input }),
    });
    const exit = yield* Effect.exit(provided(handleRunRequest(request, name)));
    if (Exit.isFailure(exit)) {
      const error = encodePublicError(Cause.squash(exit.cause));
      return { status: statusForPublicError(error), body: error };
    }
    const body: unknown = yield* Effect.promise(() => exit.value.json());
    return { status: exit.value.status, body };
  });

  let runs = 0;
  const converse = (model: ReturnType<typeof fakeText>) => provided(agentChat(caller, {
    messages: [{ role: "user", content: "go" }], threadId: `thread-${++runs}`, runId: `run-${runs}`, forwardedProps: {},
  }, model)).pipe(Effect.flatMap((stream) => Effect.promise(async () => {
    const chunks: StreamChunk[] = [];
    for await (const chunk of stream) chunks.push(chunk);
    return chunks;
  })));

  const sidebar = (name: string, input: JsonValue) => Effect.gen(function* () {
    const model = fakeText();
    model.setResponses([{ toolCalls: [{ name, input }] }, { text: "Done." }]);
    const chunks = yield* converse(model);
    const [heard, ...more] = chunks.flatMap((chunk) => chunk.type === EventType.TOOL_CALL_RESULT ? [chunk.content] : []);
    expect(more).toEqual([]);
    return Schema.decodeUnknownSync(Heard)(heard ?? expect.fail(`${name} produced no tool result`));
  });

  const listed = Effect.gen(function* () {
    const model = fakeText();
    let tools: ReadonlyArray<{ name: string; inputSchema?: unknown }> = [];
    model.setResponses([({ request }) => {
      tools = request.tools ?? [];
      return { text: "Nothing to do." };
    }]);
    yield* converse(model);
    return new Map(tools.map((tool) => [tool.name, tool.inputSchema]));
  });

  const rows = provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const upgrades = yield* drizzle.select({ channel: organizationServerUpgrades.channel, automatic: organizationServerUpgrades.automatic })
      .from(organizationServerUpgrades).where(eq(organizationServerUpgrades.organizationId, ORGANIZATION));
    const decided = yield* drizzle.select({ id: operationApprovals.id, status: operationApprovals.status, reason: operationApprovals.reason, by: operationApprovals.decidedByUserId })
      .from(operationApprovals).where(eq(operationApprovals.organizationId, ORGANIZATION)).orderBy(asc(operationApprovals.subject));
    return { upgrades, decided };
  }));

  const restore = (to: Effect.Success<typeof rows>) => provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.delete(organizationServerUpgrades).where(eq(organizationServerUpgrades.organizationId, ORGANIZATION));
    if (to.upgrades.length > 0) yield* drizzle.insert(organizationServerUpgrades).values(to.upgrades.map((row) => ({ organizationId: ORGANIZATION, ...row })));
  }));

  return { route, sidebar, listed, rows, restore, approvals: approvals.map(({ id }) => id), userId };
});

it.live("lists each agent command in the sidebar with the manifest's input schema, so the agent never calls a command with a different input than ployz", () =>
  Effect.gen(function* () {
    const { listed } = yield* parity();
    const tools = yield* listed;
    const served = new Map(manifest().commands.map((entry) => [entry["name"], entry["input"]]));
    for (const command of COMMANDS) {
      expect(tools.has(command.name), command.name).toBe(command.agents);
      if (command.agents) expect(tools.get(command.name), command.name).toEqual(served.get(command.name));
    }
  }));

it.live("gives the same answer and leaves the same rows through ployz and the sidebar, so the agent and the CLI never disagree about one command", () =>
  Effect.gen(function* () {
    const { route, sidebar, rows, restore, approvals } = yield* parity();
    const [first] = approvals;
    const fixtures: ReadonlyArray<readonly [string, JsonValue]> = [
      ["server_upgrade_policy_show", {}],
      ["server_upgrade_policy_set", { channel: "beta" }],
      ["org_approval_ls", {}],
      ["org_approval_show", { id: first ?? "" }],
    ];
    expect(fixtures.map(([name]) => name).sort()).toEqual(COMMANDS.filter((command) => command.agents).map((command) => command.name).sort());
    for (const [name, input] of fixtures) {
      const before = yield* rows;
      const viaRoute = yield* route(name, input);
      const afterRoute = yield* rows;
      yield* restore(before);
      expect(yield* rows, `${name} restored`).toEqual(before);
      const viaSidebar = yield* sidebar(name, input);
      expect(viaRoute.status, name).toBe(200);
      expect(viaSidebar, name).toEqual({ ok: true, value: asRecord(viaRoute.body)?.["output"] });
      expect(yield* rows, name).toEqual(afterRoute);
    }
    expect((yield* rows).upgrades).toEqual([{ channel: "beta", automatic: false }]);
  }));

it.live("refuses a bad input the same way through ployz and the sidebar, so the agent hears the error ployz prints", () =>
  Effect.gen(function* () {
    const { route, sidebar, rows } = yield* parity();
    const before = yield* rows;
    const viaRoute = yield* route("server_upgrade_policy_set", { channel: "alpha" });
    const viaSidebar = yield* sidebar("server_upgrade_policy_set", { channel: "alpha" });
    expect(viaRoute.status).toBe(422);
    const error = asRecord(asRecord(viaRoute.body)?.["error"]);
    expect(error).toMatchObject({ code: "invalid_input" });
    expect(viaSidebar).toEqual({ ok: false, refusal: { code: error?.["code"], message: error?.["message"] } });
    expect(yield* rows).toEqual(before);
  }));

it.live("answers approvals only through ployz, records who decided, and keeps them out of the sidebar, so the agent never approves its own request", () =>
  Effect.gen(function* () {
    const { route, sidebar, rows, approvals, userId } = yield* parity();
    const [data, logs] = approvals;
    const pending = yield* rows;
    expect(yield* sidebar("org_approval_approve", { id: data ?? "" })).toEqual({ error: "Unknown tool: org_approval_approve" });
    expect(yield* sidebar("org_approval_deny", { id: logs ?? "" })).toEqual({ error: "Unknown tool: org_approval_deny" });
    expect(yield* rows).toEqual(pending);
    const approved = yield* route("org_approval_approve", { id: data ?? "" });
    const denied = yield* route("org_approval_deny", { id: logs ?? "", reason: "not during the demo" });
    expect(approved).toMatchObject({ status: 200, body: { output: { id: data, status: "approved" }, message: "Approved: Release data. The command waiting on it goes ahead." } });
    expect(denied).toMatchObject({ status: 200, body: { output: { id: logs, status: "denied", reason: "not during the demo" }, message: "Denied: Release logs." } });
    expect((yield* rows).decided).toEqual([
      { id: data, status: "approved", reason: null, by: userId },
      { id: logs, status: "denied", reason: "not during the demo", by: userId },
    ]);
  }));
