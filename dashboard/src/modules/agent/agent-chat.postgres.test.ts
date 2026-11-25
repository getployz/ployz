import { it } from "@effect/vitest";
import type { ConfigCommand, ConfigStore, RuntimeWatchView } from "@ployz/sdk";
import { reconstructChat } from "@tanstack/ai-persistence";
import { type AnyTextAdapter, chat, defineChatMiddleware, EventType, type TokenUsage, type ModelMessage, type RunAgentResumeItem, type StreamChunk, toolDefinition } from "@tanstack/ai";
import { withPersistence } from "@tanstack/ai-persistence";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { expect, vi } from "vitest";
import { createAnthropicChat } from "@tanstack/ai-anthropic";
import { asTestDouble } from "#/lib/test-double";
import { approvalInterrupt } from "#/modules/agent/agent";
import { agentChat, REISSUE, STEP_LIMIT, STEP_LIMIT_NOTE } from "#/modules/agent/agent-chat.server";
import { AGENT_TOOLS } from "#/modules/agent/agent-runner.server";
import type { PageContext } from "#/modules/agent/page-context";
import { FAILURES } from "#/modules/agent/provider-error";
import { PostHog, type PostHogService } from "#/modules/analytics/posthog.server";
import { agentPersistence } from "#/modules/agent/persistence.server";
import { pendingApprovals } from "#/modules/approvals/approvals.server";
import { serviceAct } from "#/modules/service-act/tables";
import type { Caller } from "#/modules/identity/actor";
import { user } from "#/modules/identity/tables";
import { member } from "#/modules/organization/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { AuthLive } from "#/modules/identity/auth.server";
import { Database } from "#/server/database.server";
import { scriptedAgent } from "#/test/agent-call";
import { acmeWeb, enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloudWith } from "#/test/store-cloud";

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
  refusal: Schema.optional(Schema.Struct({ code: Schema.String, message: Schema.String, writes: Schema.optional(Schema.String) })),
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
 * model in her sidebar. `removeWeb` stages web's removal, so the next Deploy destroys it. With `asks`, Bea, a member,
 * talks instead, in a Protected Environment only Ada can approve for: a Deploy there still asks through an approval
 * card, where Ada's own Deploy would be linked to a person's tap, and Ada decides it as `decider`.
 */
const sidebar = Effect.fn(function* (options: { removeWeb: boolean; asks?: true }) {
  const stub = ConfigProvider.layer(ConfigProvider.fromEnv({ env: { PLOYZ_AGENT_STUB: "1" } }));
  const services = yield* storeTestCloudWith(Layer.merge(AuthLive, stub));
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const ownerId = yield* provided(seedStoreOrganization(ORGANIZATION));
  const userId = options.asks === undefined ? ownerId : yield* provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [person] = yield* drizzle.insert(user).values({ email: "bea@example.test", name: "Bea" }).returning();
    yield* drizzle.insert(member).values({ userId: person?.id ?? "", organizationId: ORGANIZATION, role: "member" });
    return person?.id ?? "";
  }));
  yield* provided(enrollStoreServer(ORGANIZATION));
  const store: ConfigStore = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
  if (options.asks !== undefined) yield* Effect.promise(() => store.write(ORGANIZATION, { command: "set_environment_protected", environment: here, protected: true }));
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
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, role: options.asks === undefined ? "owner" : "member", support: null, via: "dashboard" };
  const decider: Caller = { ...caller, userId: ownerId, credential: { kind: "session", id: "session-ada" }, role: "owner" };
  let runs = 0;
  const say = (content: string, forwardedProps: Forwarded = {}, model?: AnyTextAdapter) =>
    provided(agentChat(caller, { messages: [{ role: "user", content }], threadId: THREAD, runId: `run-${++runs}`, forwardedProps }, model))
      .pipe(Effect.flatMap(drain));
  const persistence = yield* provided(agentPersistence({ organizationId: ORGANIZATION, userId }));
  const waiting = Effect.promise(() => persistence.stores.interrupts.listPending(THREAD))
    .pipe(Effect.map(([first]) => first?.interruptId ?? expect.fail("nothing is waiting")));
  const answer = (interruptId: string, status: RunAgentResumeItem["status"], forwardedProps: Forwarded = {}, model?: AnyTextAdapter) =>
    provided(agentChat(caller, { messages: [], threadId: THREAD, runId: `run-${++runs}`, forwardedProps, resume: [status === "resolved" ? { interruptId, status, payload: {} } : { interruptId, status }] }, model))
      .pipe(Effect.flatMap(drain));
  const resume = (status: RunAgentResumeItem["status"], forwardedProps: Forwarded = {}, model?: AnyTextAdapter) =>
    waiting.pipe(Effect.flatMap((interruptId) => answer(interruptId, status, forwardedProps, model)));
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
  return { provided, write, store, caller, decider, userId, persistence, say, waiting, answer, resume, watchWrites, pending, deployments, rewind, ageClaim };
});

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
    const model = scriptedAgent();
    model.setResponses([{ toolCalls: [{ name: "service_add", input: { name: "api", image: "nginx" } }] }, { text: "Done." }]);
    const answered = yield* say("add an nginx service called api", { page: architecture }, model);
    expect(answered.results).toMatchObject([{ ok: true }]);
    expect(answered.said).toBe("Done.");
    const listed = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "services", environment: { project: "shop", environment: "production" } }));
    expect(JSON.stringify(listed)).toContain('"api"');
  }));

it.live("a store tool acts in the page's Environment by id after its Project was renamed under the page", () =>
  Effect.gen(function* () {
    const { say, store, write } = yield* sidebar({ removeWeb: false });
    const docs = "00000000-0000-4000-8000-00000000a607";
    const production = "00000000-0000-4000-8000-00000000a608";
    yield* write({ command: "create_project", id: docs, name: "docs", default_environment: production });
    yield* write({ command: "rename_project", project: "docs", name: "guides" });
    const model = scriptedAgent();
    model.setResponses([{ toolCalls: [{ name: "service_add", input: { name: "api", image: "nginx" } }] }, { text: "Done." }]);
    const page = { page: "architecture", project: "docs", environment: "production", environment_id: production };
    const answered = yield* say("add an nginx service called api", { page }, model);
    expect(answered.results).toMatchObject([{ ok: true }]);
    const listed = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "services", environment: { project: "guides", environment: "production" } }));
    expect(JSON.stringify(listed)).toContain('"api"');
  }));

it.live("a store tool never acts in a Project that took the page's old name when the page's Environment is gone", () =>
  Effect.gen(function* () {
    const { say, store, write } = yield* sidebar({ removeWeb: false });
    yield* write({ command: "create_project", id: "00000000-0000-4000-8000-00000000a609", name: "docs", default_environment: "00000000-0000-4000-8000-00000000a60a" });
    const model = scriptedAgent();
    model.setResponses([{ toolCalls: [{ name: "service_add", input: { name: "api", image: "nginx" } }] }, { text: "Done." }]);
    const page = { page: "architecture", project: "docs", environment: "production", environment_id: "00000000-0000-4000-8000-00000000a60b" };
    const answered = yield* say("add an nginx service called api", { page }, model);
    expect(answered.results).toMatchObject([{ ok: false, refusal: { code: "not_found" } }]);
    const listed = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "services", environment: { project: "docs", environment: "production" } }));
    expect(JSON.stringify(listed)).not.toContain('"api"');
  }));

/** Shop has only production; the page still shows a staging Environment no longer there. */
const goneStaging = { page: "architecture", project: "shop", environment: "staging", environment_id: "00000000-0000-4000-8000-00000000a60c" };

it.live("a store tool refuses on a page whose Environment is gone instead of defaulting to the only Project's", () =>
  Effect.gen(function* () {
    const { say, store } = yield* sidebar({ removeWeb: false });
    const model = scriptedAgent();
    model.setResponses([{ toolCalls: [{ name: "service_add", input: { name: "api", image: "nginx" } }] }, { text: "Done." }]);
    const answered = yield* say("add an nginx service called api", { page: goneStaging }, model);
    expect(answered.results).toMatchObject([{ ok: false, refusal: { code: "not_found" } }]);
    expect(answered.heard.join("")).toContain("shop/staging");
    const listed = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "services", environment: { project: "shop", environment: "production" } }));
    expect(JSON.stringify(listed)).not.toContain('"api"');
  }));

it.live("a store tool acts where the model names it, even on a page whose Environment is gone", () =>
  Effect.gen(function* () {
    const { say, store } = yield* sidebar({ removeWeb: false });
    const model = scriptedAgent();
    model.setResponses([
      { toolCalls: [{ name: "service_add", input: { name: "api", image: "nginx", project: "shop", environment: "production" } }] },
      { text: "Done." },
    ]);
    const answered = yield* say("add an nginx service called api to shop production", { page: goneStaging }, model);
    expect(answered.results).toMatchObject([{ ok: true }]);
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

/** Shop's one Server, fra-1, runs the only copy of web, a Global, which refuses a drain of it. */
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

const admits = (writes: ReturnType<Effect.Success<ReturnType<typeof sidebar>>["watchWrites"]>) =>
  writes.mock.calls.filter(([, command]) => command.command === "admit");

const blockFirstAdmit = ({ store, watchWrites }: Effect.Success<ReturnType<typeof sidebar>>, held: ConfigCommand["command"] = "admit") => {
  const original = store.write.bind(store);
  const writes = watchWrites();
  let enter = () => {};
  let release = () => {};
  const entered = new Promise<void>((resolve) => { enter = resolve; });
  const released = new Promise<void>((resolve) => { release = resolve; });
  let first = true;
  writes.mockImplementation(async (...args) => {
    const result = await original(...args);
    if (args[1].command === held && first) {
      first = false;
      enter();
      await released;
    }
    return result;
  });
  return { writes, entered, release: () => release() };
};

/** A Service to Save that destroys nothing, so the Save runs without waiting on a human. */
const addCache = { command: "create_service", id: "00000000-0000-4000-8000-0000000a6105", environment: here, name: "cache", image: "redis:7" } as const;

const sendAs = ({ provided, caller }: Effect.Success<ReturnType<typeof sidebar>>, runId: string, threadId = THREAD, content = "deploy") =>
  provided(agentChat(caller, { messages: [{ role: "user", content }], threadId, runId })).pipe(Effect.flatMap(drain));
const saveAs = (harness: Effect.Success<ReturnType<typeof sidebar>>, runId: string) => sendAs(harness, runId, THREAD, "save");
const publishes = (writes: ReturnType<Effect.Success<ReturnType<typeof sidebar>>["watchWrites"]>) =>
  writes.mock.calls.filter(([, command]) => command.command === "publish");

for (const status of ["completed", "aborted", "failed"] as const) {
  it.live(`a request repeating the id of a ${status} run replays that run and saves nothing more`, () =>
    Effect.gen(function* () {
      const harness = yield* sidebar({ removeWeb: false });
      const { write, persistence, watchWrites } = harness;
      yield* write(addCache);
      expect((yield* saveAs(harness, "run-once")).said).toBe("Done.");
      yield* Effect.promise(() => persistence.stores.runs.update("run-once", { status }));
      const thread = yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD));
      const writes = watchWrites();

      const again = yield* saveAs(harness, "run-once");
      expect(again.errors).toEqual([]);
      expect(again.results).toEqual([]);
      expect(again.said).toBe("");
      expect(again.shown).toContain("Done.");
      expect(writes).not.toHaveBeenCalled();
      expect(yield* Effect.promise(() => persistence.stores.messages.loadThread(THREAD))).toEqual(thread);
      expect((yield* Effect.promise(() => persistence.stores.runs.get("run-once")))?.status).toBe(status);
    }), 15_000);
}

it.live("a request repeating the id of a run still answering is refused, and only that run saves", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: false });
    yield* harness.write(addCache);
    const gate = blockFirstAdmit(harness, "publish");
    const first = Effect.runPromise(saveAs(harness, "run-busy"));
    try {
      yield* Effect.promise(() => gate.entered);
      const again = yield* saveAs(harness, "run-busy");
      expect(again.errors).toEqual(["This message is already being answered."]);
      expect(again.results).toEqual([]);
    } finally {
      gate.release();
    }
    expect((yield* Effect.promise(() => first)).said).toBe("Done.");
    expect(publishes(gate.writes)).toHaveLength(1);
  }), 15_000);

it.live("a message whose request ends before it streams leaves no run behind, so sending it again answers it", () =>
  Effect.gen(function* () {
    const harness = yield* sidebar({ removeWeb: false });
    const { provided, caller, write, persistence, watchWrites } = harness;
    yield* write(addCache);
    const writes = watchWrites();
    yield* provided(agentChat(caller, { messages: [{ role: "user", content: "save" }], threadId: THREAD, runId: "run-dropped" }));
    expect(yield* Effect.promise(() => persistence.stores.runs.get("run-dropped"))).toBeNull();

    const sent = yield* saveAs(harness, "run-dropped");
    expect(sent.errors).toEqual([]);
    expect(sent.said).toBe("Done.");
    expect(publishes(writes)).toHaveLength(1);
  }), 15_000);

it.live("two steps staged in one turn are both refused, so each lands at its own index", () =>
  Effect.gen(function* () {
    const { say, watchWrites } = yield* sidebar({ removeWeb: false });
    const writes = watchWrites();
    const model = scriptedAgent();
    model.setResponses([{ toolCalls: [
      { name: "service_stop", input: { service: "web", proposal: "00000000-0000-4000-8000-0000000a6201" } },
      { name: "service_start", input: { service: "web", proposal: "00000000-0000-4000-8000-0000000a6201" } },
    ] }, { text: "One at a time, then." }]);
    const answered = yield* say("stop and start web in one plan", { page: architecture }, model);
    expect(answered.interrupts).toEqual([]);
    expect(answered.results).toMatchObject([
      { ok: false, refusal: { code: "invalid_argument", message: "Stage one step at a time." } },
      { ok: false, refusal: { code: "invalid_argument", message: "Stage one step at a time." } },
    ]);
    expect(admits(writes)).toEqual([]);
  }));

// Catches a restart from the sidebar running without a person.
it.live("a Service restart from the sidebar answers a link to a proposal and runs nothing", () =>
  Effect.gen(function* () {
    const { provided, say } = yield* sidebar({ removeWeb: false });
    const model = scriptedAgent();
    model.setResponses([{ toolCalls: [{ name: "service_restart", input: { service: "web", wait_seconds: 0 } }] }, { text: "Waiting on a person." }]);
    const asked = yield* say("restart web", { page: architecture }, model);
    expect(asked.interrupts).toEqual([]);
    expect(asked.results).toMatchObject([{ ok: true, value: { proposal_id: expect.any(String), status: "presented", next: expect.stringContaining("proposal_status") } }]);
    expect(yield* provided(Effect.flatMap(Database, ({ drizzle }) => drizzle.select().from(serviceAct)))).toEqual([]);
  }));

/**
 * A Save an older sidebar paused on an approval card, as the chat library stored it: the thread holds the call with no
 * result, and the card's interrupt waits. Its tools fail if anything runs one.
 */
const olderCard = (persistence: Effect.Success<ReturnType<typeof agentPersistence>>) => Effect.promise(async () => {
  const model = scriptedAgent();
  model.setResponses([{ toolCalls: [{ name: "deploy", input: { skip_deploy: true } }] }]);
  const tools = AGENT_TOOLS.map((tool) => toolDefinition({ name: tool.name, description: tool.description, inputSchema: tool.inputSchema })
    .server(() => { throw new Error(`${tool.name} ran before its card was answered`); }));
  const card = defineChatMiddleware<unknown, readonly [], readonly [], typeof approvalInterrupt>({
    name: "older-approval-gate",
    onInterruptBoundary: (ctx) => {
      const call = ctx.messages.filter((message) => message.role === "assistant").at(-1)?.toolCalls?.[0];
      if (ctx.phase !== "beforeTools" || call === undefined) return undefined;
      return { interrupts: [approvalInterrupt.interrupt({ key: call.id, reason: "approval_required", message: "Approve the save.", payload: { approvalId: "00000000-0000-4000-8000-00000000a6ff" } })] };
    },
  });
  const stream = chat({ adapter: model, messages: [{ role: "user", content: "save" }], threadId: THREAD, runId: "run-older", tools, middleware: [withPersistence(persistence), card], interrupts: [approvalInterrupt] });
  for await (const _chunk of stream) { /* drained */ }
});

// Catches a card from before the upgrade running its Save once a person cancels or approves it, now that no approval stands behind it.
it.live.each(["cancelled", "resolved"] as const)("an older approval card, %s, ends its call without running it", (status) =>
  Effect.gen(function* () {
    const { write, persistence, resume, watchWrites } = yield* sidebar({ removeWeb: false });
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-0000000a6102", environment: here, name: "cache", image: "redis:7" });
    yield* olderCard(persistence);
    const writes = watchWrites();
    const model = scriptedAgent();
    model.setResponses([{ text: "Nothing ran." }]);

    const answered = yield* resume(status, {}, model);
    expect(answered.results).toEqual([status === "cancelled"
      ? { ok: false, cancelled: true }
      : { ok: false, refusal: { code: "failed_precondition", message: REISSUE } }]);
    expect(writes).not.toHaveBeenCalled();
    expect(yield* Effect.promise(() => persistence.stores.interrupts.listPending(THREAD))).toEqual([]);
  }));

it.live("a Save that destroys nothing runs without an interrupt", () =>
  Effect.gen(function* () {
    const { write, say, pending } = yield* sidebar({ removeWeb: false });
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-0000000a6102", environment: here, name: "cache", image: "redis:7" });
    const answered = yield* say("save");
    expect(answered.interrupts).toEqual([]);
    expect(answered.results).toMatchObject([{ ok: true, value: { saved: { created: true } }, nothing_destroyed: true }]);
    expect(yield* pending).toEqual([]);
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
  { name: "deploy", input: {} },
  { name: "deployment_show", input: {} },
];

// Catches a GitHub deploy that runs out of steps before its Deploy answers the link.
it.live("deploying a GitHub repository one call per turn reaches its Deploy's link within the step limit", () =>
  Effect.gen(function* () {
    const { say } = yield* sidebar({ removeWeb: false, asks: true });
    const model = scriptedAgent();
    model.setResponses([...githubDeploy.map((call) => ({ toolCalls: [call] })), { text: "api waits on approval." }]);
    const answered = yield* say("deploy acme/api", {}, model);
    expect(answered.results).toEqual(expect.arrayContaining([expect.objectContaining({ ok: true, value: expect.objectContaining({ status: "presented" }) })]));
    expect(model.state.callCount).toBe(githubDeploy.length + 1);
    expect(answered.said).toBe("api waits on approval.");
  }));

it.live("a turn that runs out of steps tells the member to say continue, and the thread keeps the note", () =>
  Effect.gen(function* () {
    const { say, persistence } = yield* sidebar({ removeWeb: false });
    const model = scriptedAgent();
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
    const model = scriptedAgent({ tokensPerSecond: 200 });
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

/**
 * The captured events as text, to search for the conversation's words, without the span ids and the parent ids that
 * point at them. Those are generated, so random base36 can spell a short word ("msg-…-qb4htwo"). Each test pins every
 * other field.
 */
const capturedText = (events: ReadonlyArray<Parameters<PostHogService["capture"]>[0]>) =>
  JSON.stringify(events.map(({ properties, ...event }) => ({ ...event, properties: { ...properties, $ai_span_id: undefined, $ai_parent_id: undefined } })));

it.live("each model call tells PostHog its tokens once, cache reads and writes apart from the rest, and none of the words", () =>
  Effect.gen(function* () {
    const { caller, say } = yield* sidebar({ removeWeb: false });
    const captured: Array<Parameters<PostHogService["capture"]>[0]> = [];
    const sink: PostHogService = {
      capture: (event) => Effect.sync(() => void captured.push(event)),
      identify: () => Effect.void,
      identifyOrganization: () => Effect.void,
    };
    const model = scriptedAgent({ cache: true });
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
    expect(capturedText(captured)).not.toMatch(/count|services|two/i);
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
    const model = scriptedAgent();
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
    expect(capturedText(captured)).not.toMatch(/look|around|quiet|secretname/i);
  }));

it.live("a tool name the catalog does not have reaches PostHog as unknown_tool, not as the model wrote it", () =>
  Effect.gen(function* () {
    const { provided, caller } = yield* sidebar({ removeWeb: false });
    const captured: Array<Parameters<PostHogService["capture"]>[0]> = [];
    const sink: PostHogService = {
      capture: (event) => Effect.sync(() => void captured.push(event)),
      identify: () => Effect.void,
      identifyOrganization: () => Effect.void,
    };
    const model = scriptedAgent();
    model.setResponses([{ toolCalls: [{ name: "my_password_is_hunter2", input: {} }] }, { text: "Done." }]);
    yield* provided(agentChat(caller, { messages: [{ role: "user", content: "hello" }], threadId: THREAD, runId: "run-unknown" }, model))
      .pipe(Effect.provideService(PostHog, sink), Effect.flatMap((stream) => Effect.promise(async () => { for await (const _ of stream); })));

    expect(JSON.stringify(captured)).not.toMatch(/hunter2/);
    expect(captured.find(({ event }) => event === "$ai_generation")?.properties).toMatchObject({ $ai_tools_called: "unknown_tool" });
  }));

// Catches a refused call reported to PostHog as if it succeeded.
it.live("a refused tool call shows in PostHog as an error", () =>
  Effect.gen(function* () {
    const { say } = yield* sidebar({ removeWeb: false });
    const captured: Array<Parameters<PostHogService["capture"]>[0]> = [];
    const sink: PostHogService = {
      capture: (event) => Effect.sync(() => void captured.push(event)),
      identify: () => Effect.void,
      identifyOrganization: () => Effect.void,
    };
    const model = scriptedAgent();
    model.setResponses([{ toolCalls: [{ name: "environment_show", input: {} }] }, { toolCalls: [{ name: "server_drain", input: { server: "fra-1", wait_seconds: 0 } }] }, { text: "A person drains it." }]);
    yield* say("drain fra-1", {}, model).pipe(lastGlobalCopy, Effect.provideService(PostHog, sink));

    expect(captured.filter(({ event }) => event === "$ai_span").map(({ properties }) => [properties?.["$ai_span_name"], properties?.["$ai_is_error"]]))
      .toEqual(expect.arrayContaining([["environment_show", false], ["server_drain", true]]));
    expect(captured.at(-1)).toMatchObject({ event: "$ai_trace", properties: { $ai_is_error: true } });
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
    const model = scriptedAgent({ tokensPerSecond: 200 });
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
    const model = scriptedAgent();
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
