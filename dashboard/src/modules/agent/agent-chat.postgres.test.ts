import { it } from "@effect/vitest";
import type { ConfigCommand, ConfigStore } from "@ployz/sdk";
import { reconstructChat } from "@tanstack/ai-persistence";
import { EventType, type ModelMessage, type RunAgentResumeItem, type StreamChunk, StreamProcessor } from "@tanstack/ai";
import { sql } from "drizzle-orm";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { expect, vi } from "vitest";
import { agentChat, KEEPALIVE_MS } from "#/modules/agent/agent-chat.server";
import { agentPersistence, CLAIM_LEASE_MS, claimResume, Superseded } from "#/modules/agent/persistence.server";
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

it.live("a resume after the Deploy committed and its process died deploys nothing more, even once the Organization stops asking", () =>
  Effect.gen(function* () {
    const { provided, store, userId, caller, say, waiting, answer, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
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
    yield* provided(setOrganizationSettings({ userId }, { organizationSlug: "shop", askBeforeDestructive: false }));

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

const sendAs = ({ provided, caller }: Effect.Success<ReturnType<typeof sidebar>>, runId: string, threadId = THREAD) =>
  provided(agentChat(caller, { messages: [{ role: "user", content: "deploy" }], threadId, runId })).pipe(Effect.flatMap(drain));

for (const status of ["completed", "aborted", "failed"] as const) {
  it.live(`a request repeating the id of a ${status} run replays that run and deploys nothing more`, () =>
    Effect.gen(function* () {
      const harness = yield* sidebar({ removeWeb: true });
      const { provided, userId, persistence, watchWrites, deployments } = harness;
      yield* provided(setOrganizationSettings({ userId }, { organizationSlug: "shop", askBeforeDestructive: false }));
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
    const harness = yield* sidebar({ removeWeb: true });
    const { provided, userId, deployments } = harness;
    yield* provided(setOrganizationSettings({ userId }, { organizationSlug: "shop", askBeforeDestructive: false }));
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
    const harness = yield* sidebar({ removeWeb: true });
    const { provided, caller, userId, persistence, deployments } = harness;
    yield* provided(setOrganizationSettings({ userId }, { organizationSlug: "shop", askBeforeDestructive: false }));
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
  it.live(`a ${command} the human denied stays denied after the Organization stops asking`, () =>
    Effect.gen(function* () {
      const { provided, caller, say, resume, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
      const approval = yield* waitingApproval(yield* say(command), pending);
      yield* provided(decideApproval(caller, approval.id, { reject: { reason: "web still serves traffic" } }));
      yield* provided(setOrganizationSettings(caller, { organizationSlug: "shop", askBeforeDestructive: false }));
      const writes = watchWrites();
      const resumed = yield* resume("resolved");
      expect(resumed.results).toMatchObject([{ ok: false, refusal: { code: "approval_denied" } }]);
      expect(writes).not.toHaveBeenCalled();
      expect(yield* deployments).toEqual([]);
    }), 15_000);
}

it.live("a cancelled approval is denied after the Organization stops asking, and a retry under it deploys nothing", () =>
  Effect.gen(function* () {
    const { provided, caller, say, resume, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(setOrganizationSettings(caller, { organizationSlug: "shop", askBeforeDestructive: false }));
    const writes = watchWrites();
    const resumed = yield* resume("cancelled");
    expect(resumed.results).toEqual([{ ok: false, cancelled: true }]);
    expect(writes).not.toHaveBeenCalled();
    expect((yield* provided(getApproval(ORGANIZATION, approval.id))).status).toBe("denied");
    expect(yield* pending).toEqual([]);
    expect(yield* deployments).toEqual([]);
  }));

it.live("a resume whose approval is still pending asks again under it after the Organization stops asking", () =>
  Effect.gen(function* () {
    const { provided, caller, say, resume, watchWrites, pending, deployments } = yield* sidebar({ removeWeb: true });
    const approval = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(setOrganizationSettings(caller, { organizationSlug: "shop", askBeforeDestructive: false }));
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

it.live("a plan that moved while it waited asks again even after the Organization stops asking", () =>
  Effect.gen(function* () {
    const { provided, caller, write, say, resume, pending, deployments } = yield* sidebar({ removeWeb: true });
    const stale = yield* waitingApproval(yield* say("deploy"), pending);
    yield* provided(decideApproval(caller, stale.id, { approve: { digest: stale.digest } }));
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-0000000a6104", environment: here, name: "cache", image: "redis:7" });
    yield* provided(setOrganizationSettings(caller, { organizationSlug: "shop", askBeforeDestructive: false }));

    const resumed = yield* resume("resolved");
    expect(resumed.results).toEqual([]);
    expect((yield* waitingApproval(resumed, pending)).id).not.toBe(stale.id);
    expect(yield* deployments).toEqual([]);
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
