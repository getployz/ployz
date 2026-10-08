import { it } from "@effect/vitest";
import type { ConfigCommand, ConfigStore } from "@ployz/sdk";
import { EventType, type RunAgentResumeItem, type StreamChunk } from "@tanstack/ai";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { expect, vi } from "vitest";
import { agentChat } from "#/modules/agent/agent-chat.server";
import { agentPersistence } from "#/modules/agent/persistence.server";
import { type ApprovalView, decideApproval, getApproval, pendingApprovals, setOrganizationSettings } from "#/modules/approvals/approvals.server";
import type { Caller } from "#/modules/identity/actor";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { acmeWeb, enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a601";
const PROJECT = "00000000-0000-4000-8000-00000000a603";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000a604";
const SERVICE = "00000000-0000-4000-8000-00000000a605";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000a606";
const here = { project: null, environment: null };
const THREAD = "thread-1";

const Outcome = Schema.fromJsonString(Schema.Struct({
  ok: Schema.Boolean,
  value: Schema.optional(Schema.Unknown),
  nothing_destroyed: Schema.optional(Schema.Literal(true)),
  cancelled: Schema.optional(Schema.Boolean),
  refusal: Schema.optional(Schema.Struct({ code: Schema.String, message: Schema.String })),
}));

/**
 * One run's stream, read the way the sidebar reads it: tool outcomes, what the model said, and what it waits on.
 * `heard` is each tool result's raw JSON, as the model reads it.
 */
const drain = (stream: AsyncIterable<StreamChunk>) => Effect.promise(async () => {
  const chunks: StreamChunk[] = [];
  for await (const chunk of stream) chunks.push(chunk);
  const heard = chunks.flatMap((chunk) => chunk.type === EventType.TOOL_CALL_RESULT ? [chunk.content] : []);
  const results = heard.map((content) => Schema.decodeUnknownSync(Outcome)(content));
  const said = chunks.flatMap((chunk) => chunk.type === EventType.TEXT_MESSAGE_CONTENT ? [chunk.delta] : []).join("");
  const interrupts = chunks.flatMap((chunk) =>
    chunk.type === EventType.RUN_FINISHED && chunk.outcome?.type === "interrupt" ? chunk.outcome.interrupts : []);
  const errors = chunks.flatMap((chunk) => chunk.type === EventType.RUN_ERROR ? [chunk.message] : []);
  const shown = chunks.flatMap((chunk) => chunk.type === EventType.MESSAGES_SNAPSHOT ? [chunk.messages] : []).at(-1);
  return { results, heard, said, interrupts, errors, shown: JSON.stringify(shown ?? []) };
});

/**
 * Shop with `web` deployed (its Applied State written as a finished Deployment would), and Ada talking to the stub
 * model in her sidebar. `removeWeb` stages web's removal, so the next Deploy destroys it.
 */
const sidebar = Effect.fn(function* (options: { removeWeb: boolean }) {
  const cloud = yield* storeTestCloud();
  const stub = ConfigProvider.layer(ConfigProvider.fromEnv({ env: { PLOYZ_AGENT_STUB: "1" } }));
  const services = yield* Layer.build(Layer.mergeAll(AuthLive.pipe(Layer.provide(cloud)), cloud, stub));
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
  yield* provided(enrollStoreServer(ORGANIZATION));
  const store: ConfigStore = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
  const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
  }, acmeWeb()));
  yield* provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update config_deployment set status = 'applied' where id = ${DEPLOYMENT}`);
    yield* drizzle.execute(sql`insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
      select environment_id, service->>'id', organization_id, ${DEPLOYMENT}, 'service', service::text
      from config_saved, lateral jsonb_array_elements(intent::jsonb->'services') service where environment_id = ${ENVIRONMENT}`);
  }));
  if (options.removeWeb) yield* write({ command: "remove_service", environment: here, service: "web" });
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" } };
  let runs = 0;
  const say = (content: string) => provided(agentChat(caller, { messages: [{ role: "user", content }], threadId: THREAD, runId: `run-${++runs}` }))
    .pipe(Effect.flatMap(drain));
  const persistence = yield* provided(agentPersistence({ organizationId: ORGANIZATION, userId }));
  const waiting = Effect.promise(() => persistence.stores.interrupts.listPending(THREAD))
    .pipe(Effect.map(([first]) => first?.interruptId ?? expect.fail("nothing is waiting")));
  const answer = (interruptId: string, status: RunAgentResumeItem["status"]) =>
    provided(agentChat(caller, { messages: [], threadId: THREAD, runId: `run-${++runs}`, resume: [status === "resolved" ? { interruptId, status, payload: {} } : { interruptId, status }] }))
      .pipe(Effect.flatMap(drain));
  const resume = (status: RunAgentResumeItem["status"]) => waiting.pipe(Effect.flatMap((interruptId) => answer(interruptId, status)));
  const watchWrites = () => vi.spyOn(store, "write");
  const pending = provided(pendingApprovals(ORGANIZATION));
  const deployments = provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.execute<{ id: string }>(sql`select id from config_deployment where id <> ${DEPLOYMENT}`, "objects");
    return rows.map((row) => row.id);
  }));
  return { provided, write, caller, userId, persistence, say, waiting, answer, resume, watchWrites, pending, deployments };
});

/** The one approval waiting in Shop, asserted to be what the interrupt names. */
const waitingApproval = Effect.fn(function* (
  asked: { interrupts: ReadonlyArray<{ id: string; reason: string }> },
  pending: Effect.Effect<ReadonlyArray<ApprovalView>, Effect.Error<ReturnType<typeof pendingApprovals>>>,
) {
  const [approval, ...others] = yield* pending;
  if (approval === undefined) return expect.fail("no approval is pending");
  expect(others).toEqual([]);
  expect(asked.interrupts).toHaveLength(1);
  expect(asked.interrupts[0]).toMatchObject({ reason: "approval_required" });
  expect(JSON.stringify(asked.interrupts[0])).toContain("ployz.approval");
  expect(JSON.stringify(asked.interrupts[0])).toContain(approval.id);
  return approval;
});

it.live("a read tool answers straight from the Store", () =>
  Effect.gen(function* () {
    const { say } = yield* sidebar({ removeWeb: false });
    const answered = yield* say("list services");
    expect(answered.interrupts).toEqual([]);
    expect(answered.results).toMatchObject([{ ok: true }]);
    expect(answered.said).toBe("Services: web.");
  }));

it.live("a turn that posts the client's whole transcript back stores each tool result once", () =>
  Effect.gen(function* () {
    const { provided, caller, userId, say } = yield* sidebar({ removeWeb: false });
    yield* say("list services");
    const persistence = yield* provided(agentPersistence({ organizationId: ORGANIZATION, userId }));
    const stored = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
    const posted = stored.map((message) => message.role === "tool" ? { ...message, id: `tool-${message.toolCallId}` } : message);
    yield* provided(agentChat(caller, { messages: [...posted, { id: "message-2", role: "user", content: "list services" }], threadId: THREAD, runId: "run-replay" }))
      .pipe(Effect.flatMap(drain));
    const saved = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
    const roles = saved.map((message) => message.role);
    expect(roles).toEqual(["user", "assistant", "tool", "assistant", "user", "assistant", "tool", "assistant"]);
  }));

it.live("a Deploy that destroys web waits on a human, and runs exactly once after they approve", () =>
  Effect.gen(function* () {
    const { provided, caller, say, resume, watchWrites, pending } = yield* sidebar({ removeWeb: true });
    const writes = watchWrites();
    const asked = yield* say("deploy");
    expect(asked.results).toEqual([]);
    const approval = yield* waitingApproval(asked, pending);
    expect(writes.mock.calls.filter(([, command]) => command.command === "admit")).toHaveLength(1);

    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    writes.mockClear();
    const resumed = yield* resume("resolved");
    expect(resumed.interrupts).toEqual([]);
    expect(resumed.results).toMatchObject([{ ok: true, value: { written: "deployment" } }]);
    expect(resumed.results[0]).not.toHaveProperty("nothing_destroyed");
    expect(writes.mock.calls.map(([, command, trusted]) => [command.command, trusted?.approval]))
      .toEqual([["admit", { approved: approval.digest }]]);
    expect(resumed.said).toBe("Done.");
  }));

const admits = (writes: ReturnType<Effect.Success<ReturnType<typeof sidebar>>["watchWrites"]>) =>
  writes.mock.calls.filter(([, command]) => command.command === "admit");

it.live("two tabs resuming one approval reach the Store once, and the later one shows the earlier one's result", () =>
  Effect.gen(function* () {
    const { provided, caller, say, waiting, answer, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const writes = watchWrites();

    const both = yield* Effect.all([answer(interruptId, "resolved"), answer(interruptId, "resolved")], { concurrency: 2 });
    expect(admits(writes)).toHaveLength(1);
    expect(yield* deployments).toHaveLength(1);
    expect(both.flatMap((tab) => tab.errors)).toEqual([]);
    expect(both.map((tab) => tab.said).sort()).toEqual(["", "Done."]);
    const later = both.find((tab) => tab.said === "") ?? expect.fail("neither tab replayed");
    expect(later.shown).toContain("Done.");
    expect(later.interrupts).toEqual([]);
  }));

it.live("resuming an approval after its turn ended replays that turn instead of deploying again", () =>
  Effect.gen(function* () {
    const { provided, caller, say, waiting, answer, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    yield* answer(interruptId, "resolved");
    const writes = watchWrites();

    const again = yield* answer(interruptId, "resolved");
    expect(admits(writes)).toEqual([]);
    expect(yield* deployments).toHaveLength(1);
    expect(again.errors).toEqual([]);
    expect(again.shown).toContain("Done.");
  }));

it.live("a turn that died after its Deploy committed deploys nothing more when resumed again", () =>
  Effect.gen(function* () {
    const { provided, caller, persistence, say, waiting, answer, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const beforeResume = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
    yield* answer(interruptId, "resolved");
    const [deployed] = yield* deployments;
    yield* Effect.promise(() => persistence.stores.messages.saveThread(THREAD, beforeResume));
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`update agent_interrupts set status = 'pending',
        record = (record - 'resolvedAt' - 'response') || '{"status":"pending"}'::jsonb where interrupt_id = ${interruptId}`);
    }));
    const writes = watchWrites();

    const again = yield* answer(interruptId, "resolved");
    expect(admits(writes)).toHaveLength(1);
    expect(again.results).toMatchObject([{ ok: true, value: { written: "deployment" } }]);
    expect(yield* deployments).toEqual([deployed]);
  }), 15_000);

it.live("a Deploy called alongside another tool is refused without reaching the Store or a human", () =>
  Effect.gen(function* () {
    const { say, watchWrites, pending } = yield* sidebar({ removeWeb: true });
    const writes = watchWrites();
    const answered = yield* say("list services and deploy");
    expect(answered.interrupts).toEqual([]);
    expect(answered.results).toMatchObject([
      { ok: true },
      { ok: false, refusal: { code: "invalid_argument", message: "Call deploy alone, in its own turn, so a human can review exactly that plan." } },
    ]);
    expect(admits(writes)).toEqual([]);
    expect(yield* pending).toEqual([]);
  }));

it.live("a denied Deploy never reaches the Store, and the agent quotes the reason instead of retrying", () =>
  Effect.gen(function* () {
    const { provided, caller, say, resume, watchWrites, pending } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { reject: { reason: "web still serves traffic" } }));

    const writes = watchWrites();
    const resumed = yield* resume("resolved");
    expect(writes).not.toHaveBeenCalled();
    expect(resumed.results).toMatchObject([{ ok: false, refusal: { code: "approval_denied" } }]);
    expect(resumed.results[0]?.refusal?.message).toContain("web still serves traffic");
    expect(resumed.heard.join("")).not.toContain(approval.id);
    expect(resumed.said).toBe("I won't retry that. A human denied this deploy: web still serves traffic");
  }));

it.live("a cancelled approval answers the agent without touching the Store", () =>
  Effect.gen(function* () {
    const { say, resume, watchWrites, pending } = yield* sidebar({ removeWeb: true });
    yield* waitingApproval(yield* say("deploy"), pending);
    const writes = watchWrites();
    const resumed = yield* resume("cancelled");
    expect(writes).not.toHaveBeenCalled();
    expect(resumed.results).toEqual([{ ok: false, cancelled: true }]);
    expect(resumed.said).toBe("The approval was cancelled, so nothing was deployed.");
  }));

it.live("a plan that moved while it waited asks again under a new approval", () =>
  Effect.gen(function* () {
    const { provided, caller, write, say, resume, pending } = yield* sidebar({ removeWeb: true });
    const stale = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, stale.id, { approve: { digest: stale.digest } }));
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-0000000a6101", environment: here, name: "cache", image: "redis:7" });

    const resumed = yield* resume("resolved");
    expect(resumed.results).toEqual([]);
    const fresh = yield* waitingApproval(resumed, pending);
    expect(fresh.id).not.toBe(stale.id);
    expect((yield* provided(getApproval(ORGANIZATION, stale.id))).status).toBe("approved");
  }));

it.live("an Organization that doesn't ask deploys the destructive plan without an interrupt", () =>
  Effect.gen(function* () {
    const { provided, userId, say, pending } = yield* sidebar({ removeWeb: true });
    yield* provided(setOrganizationSettings({ userId }, { organizationSlug: "shop", askBeforeDestructive: false }));
    const answered = yield* say("deploy");
    expect(answered.interrupts).toEqual([]);
    expect(answered.results).toMatchObject([{ ok: true, value: { written: "deployment" } }]);
    expect(answered.results[0]).not.toHaveProperty("nothing_destroyed");
    expect(yield* pending).toEqual([]);
  }));

it.live("a Deploy that destroys nothing runs without an interrupt", () =>
  Effect.gen(function* () {
    const { write, say, pending } = yield* sidebar({ removeWeb: false });
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-0000000a6102", environment: here, name: "cache", image: "redis:7" });
    const answered = yield* say("deploy");
    expect(answered.interrupts).toEqual([]);
    expect(answered.results).toMatchObject([{ ok: true, value: { written: "deployment" }, nothing_destroyed: true }]);
    expect(yield* pending).toEqual([]);
  }));
