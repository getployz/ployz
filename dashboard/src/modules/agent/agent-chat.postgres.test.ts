import { it } from "@effect/vitest";
import type { ConfigCommand, ConfigStore, RuntimeWatchView } from "@ployz/sdk";
import { reconstructChat } from "@tanstack/ai-persistence";
import { type AnyTextAdapter, EventType, type TokenUsage, type ModelMessage, type RunAgentResumeItem, type StreamChunk, StreamProcessor } from "@tanstack/ai";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { expect, vi } from "vitest";
import { fakeText } from "@tanstack/ai/testing";
import { createAnthropicChat } from "@tanstack/ai-anthropic";
import { asTestDouble } from "#/lib/test-double";
import { agentChat, KEEPALIVE_MS, STEP_LIMIT, STEP_LIMIT_NOTE } from "#/modules/agent/agent-chat.server";
import type { PageContext } from "#/modules/agent/page-context";
import { FAILURES } from "#/modules/agent/provider-error";
import { PostHog, type PostHogService } from "#/modules/analytics/posthog.server";
import { agentPersistence, CLAIM_LEASE_MS, claimResume, Superseded } from "#/modules/agent/persistence.server";
import { type ApprovalView, decideApproval, getApproval, pendingApprovals } from "#/modules/approvals/approvals.server";
import type { Caller } from "#/modules/identity/actor";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
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

/** What the sidebar forwards with a request: where the member is, well-formed or not. */
type Forwarded = { readonly page?: PageContext | Readonly<Record<string, string | number>> };

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
  return { chunks, results, heard, said, interrupts, errors, shown: JSON.stringify(shown ?? []) };
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
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, support: null };
  let runs = 0;
  const say = (content: string, forwardedProps: Forwarded = {}, model?: AnyTextAdapter) =>
    provided(agentChat(caller, { messages: [{ role: "user", content }], threadId: THREAD, runId: `run-${++runs}`, forwardedProps }, model))
      .pipe(Effect.flatMap(drain));
  const persistence = yield* provided(agentPersistence({ organizationId: ORGANIZATION, userId }));
  const waiting = Effect.promise(() => persistence.stores.interrupts.listPending(THREAD))
    .pipe(Effect.map(([first]) => first?.interruptId ?? expect.fail("nothing is waiting")));
  const answer = (interruptId: string, status: RunAgentResumeItem["status"], forwardedProps: Forwarded = {}) =>
    provided(agentChat(caller, { messages: [], threadId: THREAD, runId: `run-${++runs}`, forwardedProps, resume: [status === "resolved" ? { interruptId, status, payload: {} } : { interruptId, status }] }))
      .pipe(Effect.flatMap(drain));
  const resume = (status: RunAgentResumeItem["status"], forwardedProps: Forwarded = {}) =>
    waiting.pipe(Effect.flatMap((interruptId) => answer(interruptId, status, forwardedProps)));
  const watchWrites = () => vi.spyOn(store, "write");
  const pending = provided(pendingApprovals(ORGANIZATION));
  const deployments = provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.execute<{ id: string }>(sql`select id from config_deployment where id <> ${DEPLOYMENT}`, "objects");
    return rows.map((row) => row.id);
  }));
  const rewind = (interruptId: string, thread: ReadonlyArray<ModelMessage>) => Effect.gen(function* () {
    yield* Effect.promise(() => persistence.stores.messages.saveThread(THREAD, [...thread]));
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`update agent_interrupts set status = 'pending',
        record = (record - 'resolvedAt' - 'response') || '{"status":"pending"}'::jsonb where interrupt_id = ${interruptId}`);
    }));
  });
  const ageClaim = (interruptId: string) => provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update agent_interrupts set claimed_at = now() - interval '6 minutes' where interrupt_id = ${interruptId}`);
  }));
  return { provided, write, store, caller, userId, persistence, say, waiting, answer, resume, watchWrites, pending, deployments, rewind, ageClaim };
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

type Hydrated = { activeRun: { runId: string } | null; interrupts: { runId: string; pending: ReadonlyArray<unknown> } | null };

const reload = (persistence: Effect.Success<ReturnType<typeof agentPersistence>>) => Effect.promise(async () => {
  const response = await reconstructChat(persistence, new Request(`http://cloud.test/api/agent/shop/chat?threadId=${THREAD}`));
  return await response.json() as Hydrated;
});

/** Whether a reloaded sidebar offers the waiting approval again, as the client decides: only with no other run to rejoin. */
const reoffers = ({ activeRun, interrupts }: Hydrated) =>
  interrupts !== null && interrupts.pending.length > 0 && (activeRun === null || activeRun.runId === interrupts.runId);

it.live("a read tool answers straight from the Store", () =>
  Effect.gen(function* () {
    const { say } = yield* sidebar({ removeWeb: false });
    const answered = yield* say("list services");
    expect(answered.interrupts).toEqual([]);
    expect(answered.results).toMatchObject([{ ok: true }]);
    expect(answered.said).toBe("Services: web.");
  }));

it.live("a GitHub read of a repository the Organization can't read reaches the model as the tool's refusal", () =>
  Effect.gen(function* () {
    const { say } = yield* sidebar({ removeWeb: false });
    const answered = yield* say("tree acme/web");
    expect(answered.interrupts).toEqual([]);
    expect(answered.results).toMatchObject([{ ok: false, refusal: { code: "not_found" } }]);
    expect(answered.said).toMatch(/^The Store refused: /);
  }));

it.live("a store tool the agent mints ids for creates a Service where the member is", () =>
  Effect.gen(function* () {
    const { say, store } = yield* sidebar({ removeWeb: false });
    const model = fakeText();
    model.setResponses([{ toolCalls: [{ name: "service_add", input: { name: "api", image: "nginx" } }] }, { text: "Done." }]);
    const answered = yield* say("add an nginx service called api", { page: architecture }, model);
    expect(answered.results).toMatchObject([{ ok: true }]);
    expect(answered.said).toBe("Done.");
    const listed = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "services", environment: { project: "shop", environment: "production" } }));
    expect(JSON.stringify(listed)).toContain('"api"');
  }));

it.live("a turn Claude fails tells the member what happened in plain words, never the provider's JSON", () =>
  Effect.gen(function* () {
    const { say } = yield* sidebar({ removeWeb: false });
    const claude = createAnthropicChat("claude-sonnet-5-5", "sk-test", {
      maxRetries: 0,
      fetch: () => Promise.resolve(Response.json({
        type: "error",
        error: { type: "invalid_request_error", message: "Your credit balance is too low to access the Anthropic API." },
        request_id: "req_1",
      }, { status: 400 })),
    });
    const answered = yield* say("list services", {}, claude);
    expect(answered.errors).toEqual([FAILURES.agent_unavailable]);
    const failed = answered.chunks.find((chunk) => chunk.type === EventType.RUN_ERROR);
    expect(failed).toMatchObject({ code: "agent_unavailable" });
    expect(JSON.stringify(failed)).not.toContain("credit balance");
  }));

it.live("a Service restart runs through Cloud without asking anyone", () =>
  Effect.gen(function* () {
    const { say, pending } = yield* sidebar({ removeWeb: false });
    const model = fakeText();
    model.setResponses([{ toolCalls: [{ name: "service_restart", input: { service: "web", wait_seconds: 0 } }] }, { text: "Restarting." }]);
    const answered = yield* say("restart web", { page: architecture }, model);
    expect(answered.results).toMatchObject([{ ok: true, value: { run: "restart web", state: "accepted", at: "shop/production" } }]);
    expect(answered.said).toBe("Restarting.");
    expect(answered.interrupts).toEqual([]);
    expect(yield* pending).toEqual([]);
  }));

const architecture = { page: "architecture", project: "shop", environment: "production", service: "web" };
const ARCHITECTURE = '<dashboard-page page="architecture" project="shop" environment="production" service="web"/>';
const SERVERS = '<dashboard-page page="servers"/>';

/** Each member message's text parts, in order, as the thread stores them. */
const memberTexts = (thread: ReadonlyArray<ModelMessage>) => thread.flatMap((message) => message.role !== "user" ? [] : [
  Array.isArray(message.content) ? message.content.flatMap((part) => part.type === "text" ? [part.content] : []) : [message.content ?? ""],
]);

it.live("a member message opens with where they are only when that changed since the thread last said", () =>
  Effect.gen(function* () {
    const { persistence, say } = yield* sidebar({ removeWeb: false });
    const thread = Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
    yield* say("list services", { page: architecture });
    expect(memberTexts(yield* thread)).toEqual([[ARCHITECTURE, "list services"]]);
    const first = JSON.stringify((yield* thread)[0]);
    yield* say("list services", { page: architecture });
    yield* say("list services", { page: { page: "servers" } });
    const stored = yield* thread;
    expect(memberTexts(stored)).toEqual([[ARCHITECTURE, "list services"], ["list services"], [SERVERS, "list services"]]);
    expect(JSON.stringify(stored[0])).toBe(first);
  }));

it.live("a page the client sends malformed is left out, and the turn still answers", () =>
  Effect.gen(function* () {
    const { persistence, say } = yield* sidebar({ removeWeb: false });
    const answered = yield* say("list services", { page: { page: "", service: 7 } });
    expect(answered.said).toBe("Services: web.");
    expect(memberTexts(yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD)))).toEqual([["list services"]]);
  }));

it.live("answering an approval adds no page, even from another page", () =>
  Effect.gen(function* () {
    const { persistence, say, resume } = yield* sidebar({ removeWeb: true });
    yield* say("deploy", { page: architecture });
    yield* resume("resolved", { page: { page: "servers" } });
    expect(memberTexts(yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD)))).toEqual([[ARCHITECTURE, "deploy"]]);
  }));

it.live("the stub answers where the member is from the thread's latest page", () =>
  Effect.gen(function* () {
    const { say } = yield* sidebar({ removeWeb: false });
    expect((yield* say("Where am I?")).said).toBe("I don't know where you are.");
    expect((yield* say("where am i", { page: architecture })).said).toBe("You are on architecture in shop / production, Service web.");
    expect((yield* say("and now, where am i?")).said).toBe("You are on architecture in shop / production, Service web.");
    expect((yield* say("where am i", { page: { page: "servers", server: "srv-1" } })).said).toBe("You are on servers, Server srv-1.");
    expect((yield* say("hello", { page: { page: "deploy" } })).said).toBe("I can list services, remove one, or deploy.");
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

/** Shop's one Server, fra-1, runs the only copy of web, a Global: draining it retires web. */
const lastGlobalCopy = Effect.provideService(OrganizationRuntime, {
  cancel: () => Effect.void,
  open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({
    agentAsk: () => Effect.succeed({
      servers: [{ machine: "0".repeat(32), name: "fra-1", state: "online", release: null, upgrade: null, draining: false, disk: null, namespaces: ["shop-production"] }],
      seen: { seen_by: null, observed_at: 1, read_at: 1, age_seconds: 0, from: "server", unanswered: [] },
    }),
    watchFirstFrame: () => Effect.succeed(asTestDouble<RuntimeWatchView>()({
      machines: [{ machine: { id: "0".repeat(32), name: "fra-1", accepts_services: true, runtime: { capabilities: [] } } }],
      services: [{
        identity: "shop-production/web",
        service_id: "web",
        containers: [{
          kind: "service_container", machine_id: "0".repeat(32), runtime: { state: "running" }, created_at_unix_nanos: 1,
          resolved_spec: { mode: { mode: "global" }, volumes: [], mounts: [] },
        }],
      }],
    })),
  }) }),
});

it.live("the sidebar asks before a Drain that destroys something, even when its request came with a person's dashboard caller", () =>
  Effect.gen(function* () {
    const { provided, caller, pending } = yield* sidebar({ removeWeb: false });
    const model = fakeText();
    model.setResponses([{ toolCalls: [{ name: "server_drain", input: { server: "fra-1", wait_seconds: 0 } }] }, { text: "Draining." }]);
    const person: Caller = { ...caller, via: "dashboard" };
    const asked = yield* provided(agentChat(person, { messages: [{ role: "user", content: "drain fra-1" }], threadId: THREAD, runId: "run-drain", forwardedProps: {} }, model)
      .pipe(lastGlobalCopy, Effect.flatMap(drain)));

    const approval = yield* waitingApproval(asked, pending);
    expect(approval).toMatchObject({ status: "pending" });
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
  }), 15_000);

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
    const { provided, caller, persistence, say, waiting, answer, watchWrites, pending, deployments, rewind } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const beforeResume = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
    yield* answer(interruptId, "resolved");
    const [deployed] = yield* deployments;
    yield* rewind(interruptId, beforeResume);
    const writes = watchWrites();

    const again = yield* answer(interruptId, "resolved");
    expect(admits(writes)).toHaveLength(1);
    expect(again.results).toMatchObject([{ ok: true, value: { written: "deployment" } }]);
    expect(yield* deployments).toEqual([deployed]);
  }), 15_000);

it.live("two approved Deploys create two Deployments, and a retry of the second creates none", () =>
  Effect.gen(function* () {
    const { provided, caller, write, persistence, say, waiting, answer, pending, deployments, rewind } = yield* sidebar({ removeWeb: true });
    const first = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, first.id, { approve: { digest: first.digest } }));
    expect((yield* answer(yield* waiting, "resolved")).said).toBe("Done.");
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-0000000a6105", environment: here, name: "cache", image: "redis:7" });

    const second = yield* waitingApproval(yield* say("deploy cache"), pending);
    expect(second.id).not.toBe(first.id);
    yield* provided(decideApproval(caller, second.id, { approve: { digest: second.digest } }));
    const interruptId = yield* waiting;
    const beforeResume = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
    expect((yield* answer(interruptId, "resolved")).said).toBe("Done.");
    const deployed = yield* deployments;
    expect(new Set(deployed).size).toBe(2);

    yield* rewind(interruptId, beforeResume);
    const retried = yield* answer(interruptId, "resolved");
    expect(retried.results).toMatchObject([{ ok: true, value: { written: "deployment" } }]);
    expect([...yield* deployments].sort()).toEqual([...deployed].sort());
  }), 15_000);

it.live("a resume after the Deploy committed and its process died deploys nothing more", () =>
  Effect.gen(function* () {
    const { provided, store, caller, say, waiting, answer, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const original = store.write.bind(store);
    const writes = watchWrites();
    writes.mockImplementationOnce(async (...args) => {
      await original(...args);
      throw new Error("the process died after the Store committed");
    });
    expect((yield* answer(interruptId, "resolved").pipe(Effect.exit))._tag).toBe("Failure");
    const [deployed] = yield* deployments;

    const again = yield* answer(interruptId, "resolved");
    expect(again.errors).toEqual([]);
    expect(again.results).toMatchObject([{ ok: true, value: { written: "deployment" } }]);
    expect(yield* deployments).toEqual([deployed]);
    const [first, retried] = admits(writes).map(([, command]) => command);
    expect(retried).toEqual(first);
  }), 15_000);

const blockFirstAdmit = ({ store, watchWrites }: Effect.Success<ReturnType<typeof sidebar>>) => {
  const original = store.write.bind(store);
  const writes = watchWrites();
  let enter = () => {};
  let release = () => {};
  const entered = new Promise<void>((resolve) => { enter = resolve; });
  const released = new Promise<void>((resolve) => { release = resolve; });
  let first = true;
  writes.mockImplementation(async (...args) => {
    const result = await original(...args);
    if (args[1].command === "admit" && first) {
      first = false;
      enter();
      await released;
    }
    return result;
  });
  return { writes, entered, release: () => release() };
};

/** A Service to Deploy that destroys nothing, so the Deploy runs without waiting on a human. */
const addCache = { command: "create_service", id: "00000000-0000-4000-8000-0000000a6105", environment: here, name: "cache", image: "redis:7" } as const;

const sendAs = ({ provided, caller }: Effect.Success<ReturnType<typeof sidebar>>, runId: string, threadId = THREAD) =>
  provided(agentChat(caller, { messages: [{ role: "user", content: "deploy" }], threadId, runId })).pipe(Effect.flatMap(drain));

for (const status of ["completed", "aborted", "failed"] as const) {
  it.live(`a request repeating the id of a ${status} run replays that run and deploys nothing more`, () =>
    Effect.gen(function* () {
      const harness = yield* sidebar({ removeWeb: false });
      const { write, persistence, watchWrites, deployments } = harness;
      yield* write(addCache);
      expect((yield* sendAs(harness, "run-once")).said).toBe("Done.");
      const deployed = yield* deployments;
      expect(deployed).toHaveLength(1);
      yield* Effect.promise(() => persistence.stores.runs.update("run-once", { status }));
      const thread = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
      const writes = watchWrites();

      const again = yield* sendAs(harness, "run-once");
      expect(again.errors).toEqual([]);
      expect(again.results).toEqual([]);
      expect(again.said).toBe("");
      expect(again.shown).toContain("Done.");
      expect(writes).not.toHaveBeenCalled();
      expect(yield* deployments).toEqual(deployed);
      expect(yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD))).toEqual(thread);
      expect((yield* Effect.promise(() => persistence.stores.runs.get("run-once")))?.status).toBe(status);
    }), 15_000);
}

it.live("a request repeating the id of a run that waits on a human shows the same approval and asks nothing new", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: true });
    const { pending, watchWrites } = harness;
    const writes = watchWrites();
    const asked = yield* sendAs(harness, "run-asks");
    const approval = yield* waitingApproval(asked, pending);

    const again = yield* sendAs(harness, "run-asks");
    expect(again.interrupts).toEqual(asked.interrupts);
    expect(yield* waitingApproval(again, pending)).toEqual(approval);
    expect(admits(writes)).toHaveLength(1);
  }));

it.live("a request repeating the id of a run still answering is refused, and only that run deploys", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: false });
    const { write, deployments } = harness;
    yield* write(addCache);
    const gate = blockFirstAdmit(harness);
    const first = Effect.runPromise(sendAs(harness, "run-busy"));
    try {
      yield* Effect.promise(() => gate.entered);
      const again = yield* sendAs(harness, "run-busy");
      expect(again.errors).toEqual(["This message is already being answered."]);
      expect(again.results).toEqual([]);
    } finally {
      gate.release();
    }
    expect((yield* Effect.promise(() => first)).said).toBe("Done.");
    expect(admits(gate.writes)).toHaveLength(1);
    expect(yield* deployments).toHaveLength(1);
  }), 15_000);

it.live("a request naming another conversation's run is refused and runs nothing", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: true });
    const { pending, watchWrites } = harness;
    yield* waitingApproval(yield* sendAs(harness, "run-elsewhere"), pending);
    const writes = watchWrites();
    const other = yield* sendAs(harness, "run-elsewhere", "thread-2");
    expect(other.errors).toEqual(["This run belongs to another conversation."]);
    expect(writes).not.toHaveBeenCalled();
  }));

it.live("a message whose request ends before it streams leaves no run behind, so sending it again answers it", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: false });
    const { provided, caller, write, persistence, deployments } = harness;
    yield* write(addCache);
    yield* provided(agentChat(caller, { messages: [{ role: "user", content: "deploy" }], threadId: THREAD, runId: "run-dropped" }));
    expect(yield* Effect.promise(() => persistence.stores.runs.get("run-dropped"))).toBeNull();

    const sent = yield* sendAs(harness, "run-dropped");
    expect(sent.errors).toEqual([]);
    expect(sent.said).toBe("Done.");
    expect(yield* deployments).toHaveLength(1);
  }), 15_000);

it.live("a second request resuming the same run while the first is mid-Deploy waits for it and shows its result", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: true });
    const { provided, caller, say, waiting, pending, deployments } = harness;
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const { writes, ...gate } = blockFirstAdmit(harness);
    const resumeRun = () => Effect.runPromise(provided(agentChat(caller, {
      messages: [], threadId: THREAD, runId: "run-resumed", resume: [{ interruptId, status: "resolved", payload: {} }],
    })).pipe(Effect.flatMap(drain)));

    const first = resumeRun();
    yield* Effect.promise(() => gate.entered);
    const second = resumeRun();
    yield* Effect.sleep("600 millis");
    gate.release();
    const both = yield* Effect.promise(() => Promise.all([first, second]));

    expect(admits(writes)).toHaveLength(1);
    expect(yield* deployments).toHaveLength(1);
    expect(both.flatMap((tab) => tab.errors)).toEqual([]);
    expect(both.map((tab) => tab.said)).toEqual(["Done.", ""]);
    expect(both[1]?.shown).toContain("Done.");
    expect(yield* Effect.promise(() => harness.persistence.stores.runs.findActiveRun(THREAD))).toBeNull();
  }), 15_000);

it.live("a request waiting on another's turn keeps its stream alive with chunks the client ignores", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: true });
    const { provided, caller, say, waiting, pending } = harness;
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const gate = blockFirstAdmit(harness);
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    yield* Effect.addFinalizer(() => Effect.sync(() => vi.useRealTimers()));
    const resumeRun = () => Effect.runPromise(provided(agentChat(caller, {
      messages: [], threadId: THREAD, runId: "run-resumed", resume: [{ interruptId, status: "resolved", payload: {} }],
    })).pipe(Effect.flatMap(drain)));

    const first = resumeRun();
    yield* Effect.promise(() => gate.entered);
    const waiter = resumeRun();
    yield* Effect.sleep("300 millis");
    vi.advanceTimersByTime(KEEPALIVE_MS);
    yield* Effect.sleep("600 millis");
    gate.release();
    const [, waited] = yield* Effect.promise(() => Promise.all([first, waiter]));

    const kept = new Set<StreamChunk>(waited.chunks.filter((chunk) => chunk.type === EventType.CUSTOM));
    expect(kept.size).toBe(1);
    const rendered = (chunks: ReadonlyArray<StreamChunk>) => {
      const processor = new StreamProcessor();
      for (const chunk of chunks) processor.processChunk(chunk);
      return processor.getMessages();
    };
    expect(rendered(waited.chunks)).toEqual(rendered(waited.chunks.filter((chunk) => !kept.has(chunk))));
    expect(JSON.stringify(rendered(waited.chunks))).toContain("Done.");
  }), 15_000);

for (const [runs, stalledRun, takeoverRun] of [["one run", "run-retried", "run-retried"], ["its own run", "run-stalled", "run-takeover"]] as const) {
  it.live(`a request that takes over a stalled resume in ${runs} deploys once, and the stalled one shows the result instead of committing`, () =>
    Effect.gen(function* () {
      const harness = yield* sidebar({ removeWeb: true });
      const { provided, caller, persistence, say, waiting, pending, deployments, ageClaim } = harness;
      const approval = yield* waitingApproval(yield* say("deploy"), pending);
      yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
      const interruptId = yield* waiting;
      const { writes, ...gate } = blockFirstAdmit(harness);
      const resumeRun = (runId: string) => provided(agentChat(caller, {
        messages: [], threadId: THREAD, runId, resume: [{ interruptId, status: "resolved", payload: {} }],
      })).pipe(Effect.flatMap(drain));

      const stalled = Effect.runPromise(resumeRun(stalledRun));
      yield* Effect.promise(() => gate.entered);
      yield* ageClaim(interruptId);
      const takeover = yield* resumeRun(takeoverRun);
      yield* say("thanks");
      const thread = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
      gate.release();
      const late = yield* Effect.promise(() => stalled);

      expect(yield* deployments).toHaveLength(1);
      const ids = new Set(admits(writes).map(([, command]) => command.command === "admit" ? command.id : null));
      expect(ids.size).toBe(1);
      expect([takeover, late].flatMap((tab) => tab.errors)).toEqual([]);
      expect(takeover.said).toBe("Done.");
      expect(late.shown.split("Done.")).toHaveLength(2);
      expect(late.interrupts).toEqual([]);
      expect(yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD))).toEqual(thread);
      expect((yield* Effect.promise(() => persistence.stores.runs.get(takeoverRun)))?.status).toBe("completed");
      expect(yield* Effect.promise(() => persistence.stores.runs.findActiveRun(THREAD))).toBeNull();
    }), 15_000);
}

it.live("a run update the stalled request queued before a takeover cannot overwrite the run the takeover finished", () =>
  Effect.gen(function* () {
    const { provided, userId, say, waiting, persistence, ageClaim } = yield* sidebar({ removeWeb: true });
    yield* say("deploy");
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(yield* provided(claimResume(scope, "stalled", [interruptId]))).toBe("claimed");
    const stalled = yield* provided(agentPersistence(scope, "stalled"));
    yield* Effect.promise(() => stalled.stores.runs.createOrResume({ runId: "run-1", threadId: THREAD, startedAt: Date.now() }));
    yield* ageClaim(interruptId);

    let enter = () => {};
    let finish = () => {};
    const entered = new Promise<void>((resolve) => { enter = resolve; });
    const finished = new Promise<void>((resolve) => { finish = resolve; });
    const takeover = Effect.runPromise(provided(Effect.gen(function* () {
      const database = yield* Database;
      yield* database.transaction(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        yield* drizzle.execute(sql`select run_id from agent_runs where run_id = 'run-1' for update`);
        enter();
        yield* Effect.promise(() => finished);
        yield* drizzle.execute(sql`update agent_runs set claim = 'takeover', status = 'completed',
          record = record || '{"status":"completed"}'::jsonb where run_id = 'run-1'`);
      }));
    })));
    yield* Effect.promise(() => entered);
    const queued = stalled.stores.runs.update("run-1", { status: "failed", error: { message: "stale" } });
    const claimed = Effect.runPromise(provided(claimResume(scope, "takeover", [interruptId])));
    try {
      yield* Effect.sleep("300 millis");
      const waiters = yield* provided(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        return yield* drizzle.execute<{ n: number }>(sql`select count(*)::int as n from pg_stat_activity
          where wait_event_type = 'Lock' and query like '%update "agent_runs"%'`, "objects");
      }));
      expect(waiters[0]?.n).toBe(1);
      finish();
      yield* Effect.promise(() => Promise.all([takeover, queued]));
      expect(yield* Effect.promise(() => claimed)).toBe("claimed");
      const finishedRun = yield* Effect.promise(() => persistence.stores.runs.get("run-1"));
      expect(finishedRun?.status).toBe("completed");
      expect(JSON.stringify(finishedRun)).not.toContain("stale");
    } finally {
      finish();
      yield* Effect.promise(() => Promise.allSettled([takeover, queued, claimed]));
    }
  }), 15_000);

it.live("a request whose claim another took over can neither answer, raise, nor commit interrupts", () =>
  Effect.gen(function* () {
    const { provided, userId, say, waiting, persistence, ageClaim } = yield* sidebar({ removeWeb: true });
    yield* say("deploy");
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(yield* provided(claimResume(scope, "stalled", [interruptId]))).toBe("claimed");
    yield* ageClaim(interruptId);
    expect(yield* provided(claimResume(scope, "takeover", [interruptId]))).toBe("claimed");
    const { interrupts } = (yield* provided(agentPersistence(scope, "stalled"))).stores;
    const outcome = (write: () => Promise<void>) =>
      Effect.promise(() => write().then(() => "committed", (error: Error) => error instanceof Superseded ? "superseded" : error.message));

    expect(yield* outcome(() => interrupts.commitBatch?.([{ interruptId, status: "resolved", response: {} }]) ?? expect.fail("no commitBatch"))).toBe("superseded");
    expect(yield* outcome(() => interrupts.resolve(interruptId, {}))).toBe("superseded");
    expect(yield* outcome(() => interrupts.cancel(interruptId))).toBe("superseded");
    expect(yield* outcome(() => interrupts.create({ interruptId: "raised-late", runId: "run-1", threadId: THREAD, requestedAt: Date.now(), payload: {} }))).toBe("superseded");
    expect((yield* Effect.promise(() => persistence.stores.interrupts.listPending(THREAD))).map((record) => record.interruptId)).toEqual([interruptId]);
  }));

it.live("a takeover settles the run of a request that died holding the interrupts, so a reload rejoins nothing", () =>
  Effect.gen(function* () {
    const { provided, caller, userId, persistence, say, waiting, pending, answer, deployments, ageClaim } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(yield* provided(claimResume(scope, "dead", [interruptId]))).toBe("claimed");
    const dead = yield* provided(agentPersistence(scope, "dead"));
    yield* Effect.promise(() => dead.stores.runs.createOrResume({ runId: "run-dead", threadId: THREAD, startedAt: Date.now() }));
    yield* ageClaim(interruptId);

    const recovered = yield* answer(interruptId, "resolved");
    expect(recovered.said).toBe("Done.");
    expect(recovered.errors).toEqual([]);
    expect(yield* deployments).toHaveLength(1);
    expect(yield* Effect.promise(() => persistence.stores.interrupts.listPending(THREAD))).toEqual([]);
    expect(yield* Effect.promise(() => persistence.stores.runs.get("run-dead"))).toMatchObject({ status: "failed", error: { message: new Superseded().message } });
    expect(yield* reload(persistence)).toMatchObject({ activeRun: null, interrupts: null });
  }), 15_000);

it.live("a dead resume hides the waiting approval only until its lease lapses, and its recovery leaves later approvals offered", () =>
  Effect.gen(function* () {
    const { provided, caller, userId, persistence, say, waiting, pending, answer, deployments, ageClaim } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(reoffers(yield* reload(persistence))).toBe(true);
    expect(yield* provided(claimResume(scope, "dead", [interruptId]))).toBe("claimed");
    const dead = yield* provided(agentPersistence(scope, "dead"));
    yield* Effect.promise(() => dead.stores.runs.createOrResume({ runId: "run-dead", threadId: THREAD, startedAt: Date.now() }));
    expect((yield* reload(persistence)).activeRun).toMatchObject({ runId: "run-dead" });

    yield* ageClaim(interruptId);
    expect(reoffers(yield* reload(persistence))).toBe(true);
    const recovered = yield* answer(interruptId, "resolved");
    expect(recovered.errors).toEqual([]);
    expect(yield* deployments).toHaveLength(1);
    expect((yield* reload(persistence)).activeRun).toBeNull();
    yield* Effect.promise(() => persistence.stores.interrupts.create({ interruptId: "later", runId: "run-later", threadId: THREAD, requestedAt: Date.now(), payload: {} }));
    expect(reoffers(yield* reload(persistence))).toBe(true);
  }), 15_000);

it.live("a takeover whose request dies before its first run write leaves the approval offered and recoverable", () =>
  Effect.gen(function* () {
    const { provided, caller, userId, persistence, say, waiting, pending, answer, deployments, ageClaim } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(yield* provided(claimResume(scope, "old", [interruptId]))).toBe("claimed");
    const old = yield* provided(agentPersistence(scope, "old"));
    yield* Effect.promise(() => old.stores.runs.createOrResume({ runId: "run-old", threadId: THREAD, startedAt: Date.now() }));
    yield* ageClaim(interruptId);
    expect(yield* provided(claimResume(scope, "dies-before-run-write", [interruptId]))).toBe("claimed");
    yield* Effect.promise(() => old.stores.runs.update("run-old", { detachedSince: Date.now() }));
    yield* ageClaim(interruptId);
    expect(reoffers(yield* reload(persistence))).toBe(true);

    const recovered = yield* answer(interruptId, "resolved");
    expect(recovered.said).toBe("Done.");
    expect(recovered.errors).toEqual([]);
    expect(yield* deployments).toHaveLength(1);
    expect(yield* reload(persistence)).toMatchObject({ activeRun: null, interrupts: null });
  }), 15_000);

it.live("a request whose claim another took over cannot write its own run, which the takeover settled", () =>
  Effect.gen(function* () {
    const { provided, userId, say, waiting, persistence, ageClaim } = yield* sidebar({ removeWeb: true });
    yield* say("deploy");
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(yield* provided(claimResume(scope, "old", [interruptId]))).toBe("claimed");
    const old = yield* provided(agentPersistence(scope, "old"));
    yield* Effect.promise(() => old.stores.runs.createOrResume({ runId: "run-old", threadId: THREAD, startedAt: Date.now() }));
    yield* ageClaim(interruptId);
    expect(yield* provided(claimResume(scope, "new", [interruptId]))).toBe("claimed");
    yield* Effect.promise(() => old.stores.runs.update("run-old", { status: "failed", finishedAt: Date.now(), error: { message: "late" } }));
    const late = yield* Effect.promise(() => old.stores.runs.createOrResume({ runId: "run-late", threadId: THREAD, startedAt: Date.now() + 2 })
      .then(() => "committed", (error: Error) => error instanceof Superseded ? "superseded" : error.message));
    const fresh = yield* provided(agentPersistence(scope, "new"));
    const created = yield* Effect.promise(() => fresh.stores.runs.createOrResume({ runId: "run-new", threadId: THREAD, startedAt: Date.now() + 1 }));

    expect(late).toBe("superseded");
    expect(yield* Effect.promise(() => persistence.stores.runs.get("run-late"))).toBeNull();
    expect(created.status).toBe("running");
    expect(yield* Effect.promise(() => persistence.stores.runs.get("run-old"))).toMatchObject({ status: "failed", error: { message: new Superseded().message } });
    expect((yield* Effect.promise(() => persistence.stores.runs.findActiveRun(THREAD)))?.runId).toBe("run-new");
  }));

it.live("a takeover that resumes the run it took over runs it from a clean row, whatever the old request wrote late", () =>
  Effect.gen(function* () {
    const { provided, userId, say, waiting, persistence, ageClaim } = yield* sidebar({ removeWeb: true });
    yield* say("deploy");
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(yield* provided(claimResume(scope, "old", [interruptId]))).toBe("claimed");
    const old = yield* provided(agentPersistence(scope, "old"));
    yield* Effect.promise(() => old.stores.runs.createOrResume({ runId: "shared", threadId: THREAD, startedAt: Date.now() }));
    yield* ageClaim(interruptId);
    expect(yield* provided(claimResume(scope, "new", [interruptId]))).toBe("claimed");
    yield* Effect.promise(() => old.stores.runs.update("shared", { status: "failed", finishedAt: Date.now(), error: { message: "late" } }));
    const fresh = yield* provided(agentPersistence(scope, "new"));
    const created = yield* Effect.promise(() => fresh.stores.runs.createOrResume({ runId: "shared", threadId: THREAD, startedAt: Date.now() }));
    const during = yield* Effect.promise(() => persistence.stores.runs.findActiveRun(THREAD));
    yield* Effect.promise(() => fresh.stores.runs.update("shared", { status: "completed", finishedAt: Date.now(), detachedSince: undefined }));
    const after = yield* Effect.promise(() => persistence.stores.runs.get("shared"));

    expect(created).toMatchObject({ runId: "shared", status: "running" });
    expect(created).not.toHaveProperty("error");
    expect(created).not.toHaveProperty("finishedAt");
    expect(during).toMatchObject({ runId: "shared", status: "running" });
    expect(after?.status).toBe("completed");
    expect(after).not.toHaveProperty("error");
  }));

it.live("a takeover neither reopens nor writes over a run the old request completed while it held the interrupts", () =>
  Effect.gen(function* () {
    const { provided, userId, say, waiting, persistence, ageClaim } = yield* sidebar({ removeWeb: true });
    yield* say("deploy");
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(yield* provided(claimResume(scope, "old", [interruptId]))).toBe("claimed");
    const old = yield* provided(agentPersistence(scope, "old"));
    yield* Effect.promise(() => old.stores.runs.createOrResume({ runId: "shared", threadId: THREAD, startedAt: Date.now() }));
    yield* Effect.promise(() => old.stores.runs.update("shared", { status: "completed", finishedAt: Date.now(), detachedSince: undefined }));
    const completed = yield* Effect.promise(() => persistence.stores.runs.get("shared"));
    yield* ageClaim(interruptId);
    expect(yield* provided(claimResume(scope, "new", [interruptId]))).toBe("claimed");
    const fresh = yield* provided(agentPersistence(scope, "new"));
    const created = yield* Effect.promise(() => fresh.stores.runs.createOrResume({ runId: "shared", threadId: THREAD, startedAt: Date.now() }));
    yield* Effect.promise(() => fresh.stores.runs.update("shared", { status: "failed", finishedAt: Date.now(), error: { message: "late" } }));

    expect(completed?.status).toBe("completed");
    expect(created).toEqual(completed);
    expect(yield* Effect.promise(() => persistence.stores.runs.get("shared"))).toEqual(completed);
    expect(yield* Effect.promise(() => persistence.stores.runs.findActiveRun(THREAD))).toBeNull();
  }));

for (const status of ["aborted", "failed"] as const) {
  it.live(`a takeover leaves an unrelated ${status} run as it ended, even when asked to resume it`, () =>
    Effect.gen(function* () {
      const { provided, userId, persistence, say, waiting } = yield* sidebar({ removeWeb: true });
      yield* Effect.promise(() => persistence.stores.runs.createOrResume({ runId: "ended", threadId: THREAD, startedAt: Date.now() }));
      yield* Effect.promise(() => persistence.stores.runs.update("ended", { status, finishedAt: Date.now(), error: { message: "it ended" } }));
      const ended = yield* Effect.promise(() => persistence.stores.runs.get("ended"));
      yield* say("deploy");
      const interruptId = yield* waiting;
      const scope = { organizationId: ORGANIZATION, userId };
      expect(yield* provided(claimResume(scope, "unrelated", [interruptId]))).toBe("claimed");
      const next = yield* provided(agentPersistence(scope, "unrelated"));
      const resumed = yield* Effect.promise(() => next.stores.runs.createOrResume({ runId: "ended", threadId: THREAD, startedAt: Date.now() }));

      expect(ended).toMatchObject({ status, error: { message: "it ended" } });
      expect(resumed).toEqual(ended);
      expect(yield* Effect.promise(() => persistence.stores.runs.findActiveRun(THREAD))).toBeNull();
    }));
}

it.live("only the takeover that displaced a run may reopen it", () =>
  Effect.gen(function* () {
    const { provided, userId, say, waiting, persistence, ageClaim } = yield* sidebar({ removeWeb: true });
    yield* say("deploy");
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(yield* provided(claimResume(scope, "old", [interruptId]))).toBe("claimed");
    const old = yield* provided(agentPersistence(scope, "old"));
    yield* Effect.promise(() => old.stores.runs.createOrResume({ runId: "shared", threadId: THREAD, startedAt: Date.now() }));
    yield* ageClaim(interruptId);
    expect(yield* provided(claimResume(scope, "new", [interruptId]))).toBe("claimed");
    const displaced = yield* Effect.promise(() => persistence.stores.runs.get("shared"));
    yield* Effect.promise(() => persistence.stores.interrupts.create({ interruptId: "other", runId: "run-other", threadId: THREAD, requestedAt: Date.now(), payload: {} }));
    expect(yield* provided(claimResume(scope, "unrelated", ["other"]))).toBe("claimed");
    const unrelated = yield* provided(agentPersistence(scope, "unrelated"));
    const resumed = yield* Effect.promise(() => unrelated.stores.runs.createOrResume({ runId: "shared", threadId: THREAD, startedAt: Date.now() }));

    expect(displaced).toMatchObject({ status: "failed", error: { message: new Superseded().message } });
    expect(resumed).toEqual(displaced);
    expect(yield* Effect.promise(() => persistence.stores.runs.findActiveRun(THREAD))).toBeNull();
  }));

it.live("a same-run retry after a takeover shows as running while it works and finishes without the old error", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: true });
    const { provided, caller, userId, persistence, say, waiting, pending, deployments, ageClaim } = harness;
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const scope = { organizationId: ORGANIZATION, userId };
    expect(yield* provided(claimResume(scope, "old", [interruptId]))).toBe("claimed");
    const old = yield* provided(agentPersistence(scope, "old"));
    yield* Effect.promise(() => old.stores.runs.createOrResume({ runId: "run-shared", threadId: THREAD, startedAt: Date.now() }));
    yield* ageClaim(interruptId);
    expect(yield* provided(claimResume(scope, "dies-before-run-write", [interruptId]))).toBe("claimed");
    yield* Effect.promise(() => old.stores.runs.update("run-shared", { status: "failed", finishedAt: Date.now(), error: { message: "late" } }));
    yield* ageClaim(interruptId);
    const gate = blockFirstAdmit(harness);
    const retry = Effect.runPromise(provided(agentChat(caller, {
      messages: [], threadId: THREAD, runId: "run-shared", resume: [{ interruptId, status: "resolved", payload: {} }],
    })).pipe(Effect.flatMap(drain)));
    try {
      yield* Effect.promise(() => gate.entered);
      expect((yield* reload(persistence)).activeRun).toMatchObject({ runId: "run-shared" });
    } finally {
      gate.release();
    }
    const retried = yield* Effect.promise(() => retry);
    expect(retried.said).toBe("Done.");
    expect(yield* deployments).toHaveLength(1);
    const finished = yield* Effect.promise(() => persistence.stores.runs.get("run-shared"));
    expect(finished?.status).toBe("completed");
    expect(finished).not.toHaveProperty("error");
  }), 15_000);

it.live("a resumed turn that outlasts the lease keeps its claim, so no other request runs it again", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: true });
    const { provided, caller, userId, say, waiting, pending, deployments, ageClaim } = harness;
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const gate = blockFirstAdmit(harness);
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    yield* Effect.addFinalizer(() => Effect.sync(() => vi.useRealTimers()));
    const slow = Effect.runPromise(provided(agentChat(caller, {
      messages: [], threadId: THREAD, runId: "run-slow", resume: [{ interruptId, status: "resolved", payload: {} }],
    })).pipe(Effect.flatMap(drain)));
    yield* Effect.promise(() => gate.entered);

    yield* ageClaim(interruptId);
    vi.advanceTimersByTime(CLAIM_LEASE_MS);
    yield* Effect.sleep("300 millis");
    expect(yield* provided(claimResume({ organizationId: ORGANIZATION, userId }, "other", [interruptId]))).toBe("busy");

    gate.release();
    expect((yield* Effect.promise(() => slow)).said).toBe("Done.");
    expect(yield* deployments).toHaveLength(1);
  }), 15_000);

it.live("an answered interrupt whose request died before letting go replays at once instead of waiting out the lease", () =>
  Effect.gen(function* () {
    const { provided, userId, caller, say, waiting, answer, pending } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    yield* answer(interruptId, "resolved");
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`update agent_interrupts set claim = 'crashed', claimed_at = now() where interrupt_id = ${interruptId}`);
    }));
    expect(yield* provided(claimResume({ organizationId: ORGANIZATION, userId }, "next", [interruptId]))).toBe("settled");
  }));

it.live("an approved Publish publishes the reviewed version once, and a replay after its turn died reports that Publish", () =>
  Effect.gen(function* () {
    const { provided, caller, persistence, say, waiting, answer, watchWrites, pending, rewind } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("publish"), pending);
    if (!("diff" in approval.review)) return expect.fail("a Publish is reviewed as a diff");
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const beforeResume = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
    const writes = watchWrites();

    const published = yield* answer(interruptId, "resolved");
    expect(published.results).toEqual([{ ok: true, value: expect.objectContaining({ written: "published", created: true }) }]);
    expect(writes.mock.calls.map(([, command]) => command)).toEqual([expect.objectContaining({ command: "publish", version: approval.review.diff.version })]);

    yield* rewind(interruptId, beforeResume);
    writes.mockClear();
    const replayed = yield* answer(interruptId, "resolved");
    expect(replayed.errors).toEqual([]);
    expect(replayed.interrupts).toEqual([]);
    expect(replayed.results).toEqual([{ ok: true, value: expect.objectContaining({ written: "published", created: false }) }]);
    expect(writes.mock.calls.map(([, command]) => command.command)).toEqual(["publish"]);
    expect(yield* pending).toEqual([]);
  }), 15_000);

for (const { named, replayed } of [
  { named: (version: string) => version, replayed: { ok: true, value: expect.objectContaining({ written: "published", created: false }) } },
  { named: (version: string) => `${version}:accepted-loss`, replayed: { ok: false, refusal: expect.objectContaining({ code: "conflict" }) } },
]) {
  it.live(`an approved Publish sends ${named("V")} as the agent named it, and a replay after it landed reports ${replayed.ok ? "that Publish" : "the Store's conflict"}`, () =>
    Effect.gen(function* () {
      const { provided, caller, store, persistence, say, waiting, answer, watchWrites, pending, rewind } = yield* sidebar({ removeWeb: true });
      const { version } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "diff", environment: here }));
      const sent = named(version);
      const approval = yield* waitingApproval(yield* say(`publish --version "${sent}"`), pending);
      if (!("diff" in approval.review)) return expect.fail("a Publish is reviewed as a diff");
      expect(approval.review.diff.version).toBe(version);
      yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
      const interruptId = yield* waiting;
      const beforeResume = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
      const writes = watchWrites();

      const published = yield* answer(interruptId, "resolved");
      expect(published.results).toEqual([{ ok: true, value: expect.objectContaining({ written: "published", created: true }) }]);
      expect(writes.mock.calls.map(([, command]) => command)).toEqual([expect.objectContaining({ command: "publish", version: sent })]);

      yield* rewind(interruptId, beforeResume);
      writes.mockClear();
      const again = yield* answer(interruptId, "resolved");
      expect(again.results).toEqual([replayed]);
      expect(writes.mock.calls.map(([, command]) => command)).toEqual([expect.objectContaining({ command: "publish", version: sent })]);
    }), 15_000);
}

it.live("a replay carrying a version the human never reviewed keeps the Store's conflict, even once the reviewed Publish landed", () =>
  Effect.gen(function* () {
    const { provided, caller, persistence, say, waiting, answer, watchWrites, pending, rewind } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("publish"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const interruptId = yield* waiting;
    const beforeResume = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
    expect((yield* answer(interruptId, "resolved")).results).toMatchObject([{ ok: true, value: { written: "published", created: true } }]);

    const unreviewed = beforeResume.map((message) => message.role === "assistant" && message.toolCalls !== undefined
      ? { ...message, toolCalls: message.toolCalls.map((call) => call.function.name === "publish" ? { ...call, function: { ...call.function, arguments: '{"version":""}' } } : call) }
      : message);
    yield* rewind(interruptId, unreviewed);
    const writes = watchWrites();
    const replayed = yield* answer(interruptId, "resolved");
    expect(replayed.results).toMatchObject([{ ok: false, refusal: { code: "conflict" } }]);
    expect(writes.mock.calls.map(([, command]) => command)).toEqual([expect.objectContaining({ command: "publish", version: "" })]);
  }), 15_000);

it.live("an approved Publish resumed after someone else published is reported as a conflict, not as done", () =>
  Effect.gen(function* () {
    const { provided, caller, write, store, say, resume, pending } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("publish"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-0000000a6104", environment: here, name: "cache", image: "redis:7" });
    yield* Effect.promise(() => store.write(ORGANIZATION, { command: "publish", environment: here, version: null }));

    const resumed = yield* resume("resolved");
    expect(resumed.results).toMatchObject([{ ok: false, refusal: { code: "conflict" } }]);
    expect(resumed.said).not.toBe("Done.");
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

for (const command of ["deploy", "publish"]) {
  it.live(`a ${command} the human denied writes nothing when its run resumes`, () =>
    Effect.gen(function* () {
      const { provided, caller, say, resume, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
      const approval = yield* waitingApproval(yield* say(command), pending);
      yield* provided(decideApproval(caller, approval.id, { reject: { reason: "web still serves traffic" } }));
      const writes = watchWrites();
      const resumed = yield* resume("resolved");
      expect(resumed.results).toMatchObject([{ ok: false, refusal: { code: "approval_denied" } }]);
      expect(writes).not.toHaveBeenCalled();
      expect(yield* deployments).toEqual([]);
    }), 15_000);
}

it.live("a resume whose approval is still pending asks again under it", () =>
  Effect.gen(function* () {
    const { say, resume, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    const writes = watchWrites();
    const resumed = yield* resume("resolved");
    expect(resumed.results).toEqual([]);
    expect((yield* waitingApproval(resumed, pending)).id).toBe(approval.id);
    expect(admits(writes).map(([, , trusted]) => trusted?.approval)).toEqual(["required"]);
    expect(yield* deployments).toEqual([]);
  }), 15_000);

it.live("cancelling an approval denies it for everyone and answers the agent without touching the Store", () =>
  Effect.gen(function* () {
    const { provided, say, resume, watchWrites, pending } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    const writes = watchWrites();
    const resumed = yield* resume("cancelled");
    expect(writes).not.toHaveBeenCalled();
    expect(resumed.results).toEqual([{ ok: false, cancelled: true }]);
    expect(resumed.said).toBe("The approval was cancelled, so nothing was deployed.");
    expect(yield* provided(getApproval(ORGANIZATION, approval.id))).toMatchObject({ status: "denied", reason: null });
    expect(yield* pending).toEqual([]);
  }));

it.live("cancelling an approval someone already approved elsewhere leaves it approved and deploys nothing", () =>
  Effect.gen(function* () {
    const { provided, caller, say, resume, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const writes = watchWrites();
    const resumed = yield* resume("cancelled");
    expect(resumed.errors).toEqual([]);
    expect(resumed.results).toEqual([{ ok: false, cancelled: true }]);
    expect(writes).not.toHaveBeenCalled();
    expect((yield* provided(getApproval(ORGANIZATION, approval.id))).status).toBe("approved");
    expect(yield* deployments).toEqual([]);
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

it.live("a Deploy that destroys nothing runs without an interrupt", () =>
  Effect.gen(function* () {
    const { write, say, pending } = yield* sidebar({ removeWeb: false });
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-0000000a6102", environment: here, name: "cache", image: "redis:7" });
    const answered = yield* say("deploy");
    expect(answered.interrupts).toEqual([]);
    expect(answered.results).toMatchObject([{ ok: true, value: { written: "deployment" }, nothing_destroyed: true }]);
    expect(yield* pending).toEqual([]);
  }));

it.live("shipping a new image stages it without asking, and its Deploy runs without an interrupt", () =>
  Effect.gen(function* () {
    const { write, say, pending, deployments } = yield* sidebar({ removeWeb: false });
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-0000000a6103", environment: here, name: "api", image: "ghcr.io/acme/api:2.3" });
    const staged = yield* say("set api.image=ghcr.io/acme/api:2.4");
    expect(staged.interrupts).toEqual([]);
    expect(staged.results).toMatchObject([{ ok: true }]);
    expect(yield* pending).toEqual([]);
    const shipped = yield* say("deploy");
    expect(shipped.interrupts).toEqual([]);
    expect(shipped.results).toMatchObject([{ ok: true, value: { written: "deployment" }, nothing_destroyed: true }]);
    expect(yield* deployments).toHaveLength(1);
  }));

const githubDeploy = [
  { name: "github_ls", input: {} },
  { name: "github_ls", input: { repository: "acme/api" } },
  { name: "github_ls", input: { repository: "acme/api", match: "**/Dockerfile" } },
  { name: "github_ls", input: { repository: "acme/api", path: "Dockerfile" } },
  { name: "github_ls", input: { repository: "acme/api", path: "package.json" } },
  { name: "github_ls", input: { repository: "acme/api", path: "compose.yaml" } },
  { name: "environment_show", input: {} },
  { name: "service_add", input: { name: "api", repository: "acme/api" } },
  { name: "config_write", input: { config: "env", files: { ".env": "PORT=3000\n" }, service: "api", path: "/app" } },
  { name: "domain_add", input: { service: "api", hostname: "api.acme.dev" } },
  { name: "review", input: {} },
  { name: "publish", input: {} },
  { name: "deploy", input: {} },
  { name: "deployment_show", input: {} },
];

it.live("deploying a GitHub repository one call per turn finishes within the step limit", () =>
  Effect.gen(function* () {
    const { say } = yield* sidebar({ removeWeb: false });
    const model = fakeText();
    model.setResponses([...githubDeploy.map((call) => ({ toolCalls: [call] })), { text: "api is deploying." }]);
    const answered = yield* say("deploy acme/api", {}, model);
    expect(model.state.callCount).toBe(githubDeploy.length + 1);
    expect(answered.said).toBe("api is deploying.");
  }));

it.live("a turn that runs out of steps tells the member to say continue, and the thread keeps the note", () =>
  Effect.gen(function* () {
    const { say, persistence } = yield* sidebar({ removeWeb: false });
    const model = fakeText();
    model.setResponses(Array.from({ length: STEP_LIMIT + 5 }, () => ({ toolCalls: [{ name: "environment_show", input: {} }] })));
    const stopped = yield* say("tidy up", {}, model);
    expect(model.state.callCount).toBe(STEP_LIMIT);
    expect(stopped.said).toBe(STEP_LIMIT_NOTE);
    const thread = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
    expect(thread.at(-1)).toMatchObject({ role: "assistant", content: STEP_LIMIT_NOTE });
    expect(yield* Effect.promise(() => persistence.stores.runs.findActiveRun(THREAD))).toBeNull();

    model.setResponses([{ text: "Picking up." }]);
    expect((yield* say("continue", {}, model)).said).toBe("Picking up.");
  }));

it.live("a member who stops a reply mid-stream ends the run, and the thread answers the next message", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: false });
    const { provided, caller, persistence, say } = harness;
    const model = fakeText({ tokensPerSecond: 200 });
    model.setResponses([{ text: "Here is a long answer. ".repeat(40) }]);
    const abortController = new AbortController();
    const stream = yield* provided(agentChat(caller, { messages: [{ role: "user", content: "explain" }], threadId: THREAD, runId: "run-stopped", abortController }, model));
    const heard = yield* Effect.promise(async () => {
      let deltas = 0;
      for await (const chunk of stream) {
        if (chunk.type === EventType.TEXT_MESSAGE_CONTENT && ++deltas === 3) abortController.abort();
      }
      return deltas;
    });

    expect(heard).toBeLessThan(10);
    expect((yield* Effect.promise(() => persistence.stores.runs.get("run-stopped")))?.status).toBe("aborted");
    expect(yield* Effect.promise(() => persistence.stores.runs.findActiveRun(THREAD))).toBeNull();
    model.setResponses([{ text: "Short, then." }]);
    expect((yield* say("shorter please", {}, model)).said).toBe("Short, then.");
  }));

it.live("each model call tells PostHog its tokens once, cache reads and writes apart from the rest, and none of the words", () =>
  Effect.gen(function* () {
    const { caller, say } = yield* sidebar({ removeWeb: false });
    const captured: Array<Parameters<PostHogService["capture"]>[0]> = [];
    const sink: PostHogService = {
      capture: (event) => Effect.sync(() => void captured.push(event)),
      identify: () => Effect.void,
      identifyOrganization: () => Effect.void,
    };
    const model = fakeText({ cache: true });
    const reported: TokenUsage[] = [];
    const stream = model.chatStream.bind(model);
    model.chatStream = async function* (options) {
      for await (const chunk of stream(options)) {
        if (chunk.type === EventType.RUN_FINISHED && chunk.usage !== undefined && !Array.isArray(chunk.usage)) reported.push(chunk.usage);
        yield chunk;
      }
    };
    model.setResponses([{ text: "Two services." }, { text: "Still two." }]);
    yield* say("count services", {}, model).pipe(Effect.provideService(PostHog, sink));
    yield* say("count them again", {}, model).pipe(Effect.provideService(PostHog, sink));

    expect(reported[1]?.promptTokensDetails?.cachedTokens).toBeGreaterThan(0);
    expect(captured).toEqual(reported.flatMap((usage, index) => {
      const cacheRead = usage.promptTokensDetails?.cachedTokens ?? 0;
      const cacheWrite = usage.promptTokensDetails?.cacheWriteTokens ?? 0;
      const run = `run-${index + 1}`;
      return [{
        userId: caller.userId,
        organizationId: ORGANIZATION,
        event: "$ai_generation",
        properties: {
          $ai_trace_id: run,
          $ai_session_id: THREAD,
          $ai_span_id: expect.any(String),
          $ai_parent_id: run,
          $ai_provider: "fake",
          $ai_model: "fake-model",
          $ai_input_tokens: usage.promptTokens - cacheRead - cacheWrite,
          $ai_output_tokens: usage.completionTokens,
          $ai_cache_read_input_tokens: cacheRead,
          $ai_cache_creation_input_tokens: cacheWrite,
          $ai_cache_reporting_exclusive: true,
          $ai_latency: expect.any(Number),
          $ai_stream: true,
        },
      }, {
        userId: caller.userId,
        organizationId: ORGANIZATION,
        event: "$ai_trace",
        properties: { $ai_trace_id: run, $ai_session_id: THREAD, $ai_span_name: "sidebar_turn", $ai_latency: expect.any(Number), $ai_is_error: false },
      }];
    }));
    expect(JSON.stringify(captured)).not.toMatch(/count|services|two/i);
  }));

it.live("a turn's trace in PostHog nests each tool call under the model call that made it, and none of the words", () =>
  Effect.gen(function* () {
    const { caller, say } = yield* sidebar({ removeWeb: false });
    const captured: Array<Parameters<PostHogService["capture"]>[0]> = [];
    const sink: PostHogService = {
      capture: (event) => Effect.sync(() => void captured.push(event)),
      identify: () => Effect.void,
      identifyOrganization: () => Effect.void,
    };
    const model = fakeText();
    model.setResponses([{ toolCalls: [{ name: "environment_show", input: { environment: "secretname" } }] }, { text: "All quiet." }]);
    yield* say("look around", {}, model).pipe(Effect.provideService(PostHog, sink));

    const envelope = { userId: caller.userId, organizationId: ORGANIZATION };
    const generation = {
      $ai_trace_id: "run-1",
      $ai_session_id: THREAD,
      $ai_parent_id: "run-1",
      $ai_provider: "fake",
      $ai_model: "fake-model",
      $ai_input_tokens: expect.any(Number),
      $ai_output_tokens: expect.any(Number),
      $ai_cache_read_input_tokens: 0,
      $ai_cache_creation_input_tokens: 0,
      $ai_cache_reporting_exclusive: true,
      $ai_latency: expect.any(Number),
      $ai_stream: true,
    };
    expect(captured).toEqual([
      { ...envelope, event: "$ai_generation", properties: { ...generation, $ai_span_id: expect.any(String), $ai_tools_called: "environment_show", $ai_tool_call_count: 1 } },
      {
        ...envelope,
        event: "$ai_span",
        properties: {
          $ai_trace_id: "run-1",
          $ai_session_id: THREAD,
          $ai_span_id: expect.any(String),
          $ai_parent_id: captured[0]?.properties?.["$ai_span_id"],
          $ai_span_name: "environment_show",
          $ai_latency: expect.any(Number),
          $ai_is_error: true,
        },
      },
      { ...envelope, event: "$ai_generation", properties: { ...generation, $ai_span_id: expect.any(String) } },
      { ...envelope, event: "$ai_trace", properties: { $ai_trace_id: "run-1", $ai_session_id: THREAD, $ai_span_name: "sidebar_turn", $ai_latency: expect.any(Number), $ai_is_error: true } },
    ]);
    expect(captured[0]?.properties?.["$ai_span_id"]).not.toBe(captured[2]?.properties?.["$ai_span_id"]);
    expect(JSON.stringify(captured)).not.toMatch(/look|around|quiet|secretname/i);
  }));

it.live("a refused tool call shows in PostHog as an error", () =>
  Effect.gen(function* () {
    const { say } = yield* sidebar({ removeWeb: true });
    const captured: Array<Parameters<PostHogService["capture"]>[0]> = [];
    const sink: PostHogService = {
      capture: (event) => Effect.sync(() => void captured.push(event)),
      identify: () => Effect.void,
      identifyOrganization: () => Effect.void,
    };
    yield* say("list services and deploy").pipe(Effect.provideService(PostHog, sink));

    expect(captured.filter(({ event }) => event === "$ai_span").map(({ properties }) => [properties?.["$ai_span_name"], properties?.["$ai_is_error"]]))
      .toEqual(expect.arrayContaining([["environment_show", false], ["deploy", true]]));
    expect(captured.at(-1)).toMatchObject({ event: "$ai_trace", properties: { $ai_is_error: true } });
  }));

it.live("an approved call finished by a resumed run hangs under that run's trace in PostHog", () =>
  Effect.gen(function* () {
    const { provided, caller, say, resume, pending } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("publish"), pending);
    yield* provided(decideApproval(caller, approval.id, { approve: { digest: approval.digest } }));
    const captured: Array<Parameters<PostHogService["capture"]>[0]> = [];
    const sink: PostHogService = {
      capture: (event) => Effect.sync(() => void captured.push(event)),
      identify: () => Effect.void,
      identifyOrganization: () => Effect.void,
    };
    yield* resume("resolved").pipe(Effect.provideService(PostHog, sink));

    const traced = captured.filter(({ event }) => event.startsWith("$ai_"));
    expect(traced[0]).toMatchObject({
      event: "$ai_span",
      properties: { $ai_trace_id: "run-2", $ai_parent_id: "run-2", $ai_span_name: "publish", $ai_is_error: false },
    });
    expect(traced.at(-1)).toMatchObject({ event: "$ai_trace", properties: { $ai_trace_id: "run-2", $ai_is_error: false } });
  }), 15_000);

it.live("a tool name the catalog does not have reaches PostHog as unknown_tool, not as the model wrote it", () =>
  Effect.gen(function* () {
    const { provided, caller } = yield* sidebar({ removeWeb: false });
    const captured: Array<Parameters<PostHogService["capture"]>[0]> = [];
    const sink: PostHogService = {
      capture: (event) => Effect.sync(() => void captured.push(event)),
      identify: () => Effect.void,
      identifyOrganization: () => Effect.void,
    };
    const model = fakeText();
    model.setResponses([{ toolCalls: [{ name: "my_password_is_hunter2", input: {} }] }, { text: "Done." }]);
    yield* provided(agentChat(caller, { messages: [{ role: "user", content: "hello" }], threadId: THREAD, runId: "run-unknown" }, model))
      .pipe(Effect.provideService(PostHog, sink), Effect.flatMap((stream) => Effect.promise(async () => { for await (const _ of stream); })));

    expect(JSON.stringify(captured)).not.toMatch(/hunter2/);
    expect(captured.find(({ event }) => event === "$ai_generation")?.properties).toMatchObject({ $ai_tools_called: "unknown_tool" });
  }));

it.live("a reply stopped mid-stream still closes its trace in PostHog", () =>
  Effect.gen(function* () {
    const { provided, caller } = yield* sidebar({ removeWeb: false });
    const captured: Array<Parameters<PostHogService["capture"]>[0]> = [];
    const sink: PostHogService = {
      capture: (event) => Effect.sync(() => void captured.push(event)),
      identify: () => Effect.void,
      identifyOrganization: () => Effect.void,
    };
    const model = fakeText({ tokensPerSecond: 200 });
    model.setResponses([{ text: "Here is a long answer. ".repeat(40) }]);
    const abortController = new AbortController();
    const stream = yield* provided(agentChat(caller, { messages: [{ role: "user", content: "explain" }], threadId: THREAD, runId: "run-stopped", abortController }, model))
      .pipe(Effect.provideService(PostHog, sink));
    yield* Effect.promise(async () => {
      for await (const chunk of stream) if (chunk.type === EventType.TEXT_MESSAGE_CONTENT) abortController.abort();
    });

    expect(captured.filter(({ event }) => event === "$ai_trace")).toEqual([
      expect.objectContaining({ properties: { $ai_trace_id: "run-stopped", $ai_session_id: THREAD, $ai_span_name: "sidebar_turn", $ai_latency: expect.any(Number), $ai_is_error: false } }),
    ]);
  }));

it.live("Claude thinks as it sees fit and sends a summary of its thinking back", () =>
  Effect.gen(function* () {
    const { caller, provided } = yield* sidebar({ removeWeb: false });
    const sent: unknown[] = [];
    vi.stubGlobal("fetch", vi.fn(async (_url: string, init?: RequestInit) => {
      sent.push(JSON.parse(String(init?.body)));
      return Response.json({ type: "error", error: { type: "invalid_request_error", message: "test" } }, { status: 400 });
    }));
    const claude = ConfigProvider.layer(ConfigProvider.fromEnv({ env: { ANTHROPIC_API_KEY: "sk-ant-test" } }));
    yield* provided(agentChat(caller, { messages: [{ role: "user", content: "count services" }], threadId: THREAD, runId: "run-claude" })
      .pipe(Effect.provide(claude))).pipe(Effect.flatMap(drain), Effect.ensuring(Effect.sync(() => vi.unstubAllGlobals())));

    expect(sent).toEqual([expect.objectContaining({ thinking: { type: "adaptive", display: "summarized" } })]);
  }));

it.live("a reply's thinking is kept with the thread and comes back on reload, before its words", () =>
  Effect.gen(function* () {
    const { persistence, say } = yield* sidebar({ removeWeb: false });
    const model = fakeText();
    model.setResponses([{ thinking: "The thread names two services.", text: "Two services." }]);
    yield* say("count services", {}, model);

    const { messages } = yield* Effect.promise(async () => {
      const response = await reconstructChat(persistence, new Request(`http://cloud.test/api/agent/shop/chat?threadId=${THREAD}`));
      return await response.json() as { messages: ReadonlyArray<{ role: string; parts: ReadonlyArray<{ type: string; content?: string }> }> };
    });
    expect(messages.filter(({ role }) => role === "assistant").flatMap(({ parts }) => parts)
      .filter(({ type }) => type === "thinking" || type === "text").map(({ type, content }) => [type, content]))
      .toEqual([["thinking", "The thread names two services."], ["text", "Two services."]]);
  }));
