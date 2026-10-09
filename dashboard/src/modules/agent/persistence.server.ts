import "@tanstack/react-start/server-only";
import { isRunStatus, isTerminalRunStatus, type ModelMessage, type RunError, type RunRecord, type RunStatus, type RunStore } from "@tanstack/ai";
import type { ChatWithInterruptsPersistence, InterruptRecord, InterruptStore, MessageStore } from "@tanstack/ai-persistence";
import { and, asc, desc, eq, exists, gt, inArray, isNull, notInArray, or, type SQL, sql } from "drizzle-orm";
import type { PgColumn, PgUpdateSetSource } from "drizzle-orm/pg-core";
import { Effect } from "effect";
import { agentInterrupts, agentRuns, agentThreads } from "#/modules/agent/tables";
import { Database } from "#/server/database.server";

/** Whose sidebar a store reads and writes: one member in one Organization. Nothing outside it is visible. */
export type AgentScope = { readonly organizationId: string; readonly userId: string };

/** `record` with `patch` merged in, a key whose patch value is `undefined` removed, as `{ ...record, ...patch }` reads. */
const merged = (record: PgColumn, patch: Partial<RunRecord> | Partial<InterruptRecord>): SQL => {
  const entries = Object.entries(patch);
  const removed = entries.filter(([, value]) => value === undefined).map(([key]) => sql`${key}::text`);
  const set = Object.fromEntries(entries.filter(([, value]) => value !== undefined));
  return sql`(${sql.join([sql`${record}`, ...removed], sql` - `)}) || ${JSON.stringify(set)}::jsonb`;
};

/** A takeover resuming the run it took over starts it clean, whatever the old claim wrote late. */
const resumed: Partial<RunRecord> = { status: "running", finishedAt: undefined, error: undefined, detachedSince: undefined };

/** A resumed turn lost its interrupts to another request, which now owns the turn and its result. */
export class Superseded extends Error {
  constructor() {
    super("Another request took over this resumed turn.");
  }
}

const STATUSES = Object.keys({ running: null, interrupted: null, completed: null, failed: null, aborted: null } satisfies Record<RunStatus, null>);
const TERMINAL = STATUSES.filter(isRunStatus).filter(isTerminalRunStatus);

const supersededError = { message: new Superseded().message, code: "superseded" } satisfies RunError;
const displaced = and(eq(agentRuns.status, "failed"), sql`${agentRuns.record}->'error'->>'code' = ${supersededError.code}`);

/** How long a resuming request holds its interrupts unrenewed before another may take them over: one that died mid-turn. */
export const CLAIM_LEASE_MS = 5 * 60 * 1000;

/**
 * TanStack AI's messages, runs and interrupts stores over Postgres, confined to `scope`. With `claim`, the stores serve
 * a resumed turn: its transcript and its answers commit only while that claim still holds the interrupts it resumes.
 */
export const agentPersistence = Effect.fn("Agent.persistence")(function* (scope: AgentScope, claim?: string) {
  const run = Effect.runPromiseWith(yield* Effect.context<Database>());
  const database = yield* Database;
  const { drizzle } = database;
  const threadOwned = and(eq(agentThreads.organizationId, scope.organizationId), eq(agentThreads.userId, scope.userId));
  const runOwned = and(eq(agentRuns.organizationId, scope.organizationId), eq(agentRuns.userId, scope.userId));
  const interruptOwned = and(eq(agentInterrupts.organizationId, scope.organizationId), eq(agentInterrupts.userId, scope.userId));
  const held = claim === undefined ? undefined : eq(agentInterrupts.claim, claim);

  /**
   * `write` committed only while `claim` still holds the interrupts it resumes. The shared lock on them serializes it
   * with a takeover, which locks them for update before it moves the claim.
   */
  const holding = async <A, E>(write: Effect.Effect<A, E, Database>) => {
    if (held === undefined) return { held: true, value: await run(write) } as const;
    return run(database.transaction(Effect.gen(function* () {
      const holds = yield* drizzle.select({ id: agentInterrupts.interruptId }).from(agentInterrupts)
        .where(and(interruptOwned, held)).limit(1).for("share");
      return holds.length === 0 ? { held: false } as const : { held: true, value: yield* write } as const;
    })));
  };
  const fenced = async <A, E>(write: Effect.Effect<A, E, Database>) => {
    const outcome = await holding(write);
    if (!outcome.held) throw new Superseded();
    return outcome.value;
  };

  const messages: MessageStore = {
    loadThread: async (threadId: string) => {
      const [row] = await run(drizzle.select({ messages: agentThreads.messages }).from(agentThreads)
        .where(and(threadOwned, eq(agentThreads.threadId, threadId))));
      return row?.messages ?? [];
    },
    saveThread: async (threadId: string, saved: Array<ModelMessage>) => {
      const [row] = await fenced(drizzle.insert(agentThreads).values({ threadId, ...scope, messages: saved })
        .onConflictDoUpdate({ target: agentThreads.threadId, set: { messages: saved, updatedAt: new Date() }, setWhere: threadOwned })
        .returning({ threadId: agentThreads.threadId }));
      if (row === undefined) throw new Error(`Thread ${threadId} belongs to another member.`);
    },
  };

  const getRun = async (runId: string) => {
    const [row] = await run(drizzle.select({ record: agentRuns.record }).from(agentRuns).where(and(runOwned, eq(agentRuns.runId, runId))));
    return row?.record ?? null;
  };
  const listRuns = async (where: SQL | undefined) => {
    const rows = await run(drizzle.select({ record: agentRuns.record }).from(agentRuns)
      .where(and(runOwned, where)).orderBy(asc(agentRuns.startedAt), asc(agentRuns.runId)));
    return rows.map((row) => row.record);
  };
  const runs = {
    createOrResume: async (input) => {
      const record: RunRecord = {
        runId: input.runId,
        threadId: input.threadId,
        status: input.status ?? "running",
        startedAt: input.startedAt,
      };
      if (input.parentRunId !== undefined) record.parentRunId = input.parentRunId;
      if (input.subagentRunId !== undefined) record.subagentRunId = input.subagentRunId;
      if (input.name !== undefined) record.name = input.name;
      const insert = drizzle.insert(agentRuns).values({ ...scope, runId: record.runId, threadId: record.threadId, status: record.status, startedAt: record.startedAt, record, claim });
      await fenced(claim === undefined
        ? insert.onConflictDoNothing()
        : insert.onConflictDoUpdate({
          target: agentRuns.runId,
          set: { claim, status: "running", record: merged(agentRuns.record, resumed), updatedAt: new Date() },
          setWhere: and(runOwned, or(notInArray(agentRuns.status, TERMINAL), and(eq(agentRuns.claim, claim), displaced))),
        }));
      const existing = await getRun(input.runId);
      if (existing === null) throw new Error(`Run ${input.runId} belongs to another member.`);
      return existing;
    },
    update: async (runId, patch) => {
      const set: PgUpdateSetSource<typeof agentRuns> = { record: merged(agentRuns.record, patch), updatedAt: new Date() };
      if (patch.status !== undefined) set.status = patch.status;
      const driving = claim === undefined ? undefined : eq(agentRuns.claim, claim);
      await holding(drizzle.update(agentRuns).set(set).where(and(runOwned, eq(agentRuns.runId, runId), driving)));
    },
    get: getRun,
    findActiveRun: async (threadId) => {
      const leased = gt(agentInterrupts.claimedAt, new Date(Date.now() - CLAIM_LEASE_MS));
      const driven = or(isNull(agentRuns.claim), exists(drizzle.select({ id: agentInterrupts.interruptId }).from(agentInterrupts)
        .where(and(interruptOwned, eq(agentInterrupts.claim, agentRuns.claim), leased))));
      const [row] = await run(drizzle.select({ record: agentRuns.record }).from(agentRuns)
        .where(and(runOwned, eq(agentRuns.threadId, threadId), eq(agentRuns.status, "running"), driven))
        .orderBy(desc(agentRuns.startedAt), desc(agentRuns.runId)).limit(1));
      return row?.record ?? null;
    },
    listByThread: (threadId) => listRuns(eq(agentRuns.threadId, threadId)),
    listByParentRun: (parentRunId) => listRuns(sql`${agentRuns.record}->>'parentRunId' = ${parentRunId}`),
    listReclaimable: ({ now, ttlMs }) => listRuns(and(
      eq(agentRuns.status, "running"),
      sql`(${agentRuns.record}->>'detachedSince')::bigint <= ${now - ttlMs}`,
    )),
  } satisfies RunStore;

  const listInterrupts = async (where: SQL | undefined) => {
    const rows = await run(drizzle.select({ record: agentInterrupts.record }).from(agentInterrupts)
      .where(and(interruptOwned, where)).orderBy(asc(agentInterrupts.requestedAt), asc(agentInterrupts.interruptId)));
    return rows.map((row) => row.record);
  };
  const settle = (interruptId: string, patch: Pick<InterruptRecord, "status" | "resolvedAt" | "response">) =>
    drizzle.update(agentInterrupts)
      .set({ status: patch.status, record: merged(agentInterrupts.record, patch), updatedAt: new Date() })
      .where(and(interruptOwned, eq(agentInterrupts.interruptId, interruptId)));
  const pending = eq(agentInterrupts.status, "pending");
  const interrupts: InterruptStore = {
    create: async (input) => {
      const record: InterruptRecord = { ...input, status: "pending" };
      await fenced(drizzle.insert(agentInterrupts).values({
        ...scope,
        interruptId: record.interruptId,
        runId: record.runId,
        threadId: record.threadId,
        status: record.status,
        requestedAt: record.requestedAt,
        record,
      }).onConflictDoNothing());
    },
    resolve: async (interruptId, response) => {
      await fenced(settle(interruptId, { status: "resolved", resolvedAt: Date.now(), response }));
    },
    cancel: async (interruptId) => {
      await fenced(settle(interruptId, { status: "cancelled", resolvedAt: Date.now() }));
    },
    commitBatch: async (entries) => {
      const outcome = await run(database.transaction(Effect.gen(function* () {
        const ids = entries.map((entry) => entry.interruptId);
        if (new Set(ids).size !== ids.length) return yield* Effect.die(new Error("An interrupt batch names one interrupt twice."));
        const found = yield* drizzle.select({ id: agentInterrupts.interruptId }).from(agentInterrupts)
          .where(and(interruptOwned, pending, held, inArray(agentInterrupts.interruptId, ids))).for("update");
        if (found.length !== ids.length && held !== undefined) return "superseded" as const;
        if (found.length !== ids.length) return yield* Effect.die(new Error("An interrupt batch names an interrupt that is not pending."));
        const resolvedAt = Date.now();
        for (const entry of entries) {
          yield* settle(entry.interruptId, entry.status === "resolved"
            ? { status: "resolved", resolvedAt, response: entry.response }
            : { status: "cancelled", resolvedAt });
        }
        return "committed" as const;
      })));
      if (outcome === "superseded") throw new Superseded();
    },
    get: async (interruptId) => (await listInterrupts(eq(agentInterrupts.interruptId, interruptId)))[0] ?? null,
    list: (threadId) => listInterrupts(eq(agentInterrupts.threadId, threadId)),
    listPending: (threadId) => listInterrupts(and(pending, eq(agentInterrupts.threadId, threadId))),
    listByRun: (runId) => listInterrupts(eq(agentInterrupts.runId, runId)),
    listPendingByRun: (runId) => listInterrupts(and(pending, eq(agentInterrupts.runId, runId))),
  };

  return { stores: { messages, runs, interrupts } } satisfies ChatWithInterruptsPersistence;
});

/** Whether `threadId` is free or already `scope`'s: another member's thread is never readable or writable. */
export const threadAvailable = Effect.fn("Agent.threadAvailable")(function* (scope: AgentScope, threadId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ organizationId: agentThreads.organizationId, userId: agentThreads.userId })
    .from(agentThreads).where(eq(agentThreads.threadId, threadId));
  return row === undefined || (row.organizationId === scope.organizationId && row.userId === scope.userId);
});

const owned = (scope: AgentScope) => and(eq(agentInterrupts.organizationId, scope.organizationId), eq(agentInterrupts.userId, scope.userId));

/**
 * Whether the request holding `claim` may answer `interruptIds`: `settled` once they are all answered, `busy` while
 * another request holds one, `claimed` once it holds every pending one, and `unchecked` when one is missing so the chat
 * rejects the resume.
 */
export const claimResume = Effect.fn("Agent.claimResume")(function* (scope: AgentScope, claim: string, interruptIds: ReadonlyArray<string>) {
  const database = yield* Database;
  const { drizzle } = database;
  return yield* database.transaction(Effect.gen(function* () {
    const ids = [...new Set(interruptIds)];
    const rows = yield* drizzle.select({ status: agentInterrupts.status, claim: agentInterrupts.claim, claimedAt: agentInterrupts.claimedAt })
      .from(agentInterrupts).where(and(owned(scope), inArray(agentInterrupts.interruptId, ids))).for("update");
    if (rows.length < ids.length) return "unchecked" as const;
    if (rows.every((row) => row.status !== "pending")) return "settled" as const;
    const leased = new Date(Date.now() - CLAIM_LEASE_MS);
    if (rows.some((row) => row.claim !== null && row.claim !== claim && row.claimedAt !== null && row.claimedAt > leased)) return "busy" as const;
    yield* drizzle.update(agentInterrupts).set({ claim, claimedAt: new Date() })
      .where(and(owned(scope), inArray(agentInterrupts.interruptId, ids), eq(agentInterrupts.status, "pending")));
    const superseded = rows.flatMap((row) => row.status === "pending" && row.claim !== null && row.claim !== claim ? [row.claim] : []);
    if (superseded.length > 0) {
      const moved = and(eq(agentRuns.organizationId, scope.organizationId), eq(agentRuns.userId, scope.userId), inArray(agentRuns.claim, superseded));
      yield* drizzle.update(agentRuns).set({ claim, updatedAt: new Date() }).where(and(moved, displaced));
      yield* drizzle.update(agentRuns).set({
        claim,
        status: "failed",
        record: merged(agentRuns.record, { status: "failed", finishedAt: Date.now(), error: supersededError }),
        updatedAt: new Date(),
      }).where(and(moved, eq(agentRuns.status, "running")));
    }
    return "claimed" as const;
  }));
});

/** Keeps `claim`'s hold on its interrupts fresh while its turn runs, so no one takes over a turn that is still alive. */
export const renewResume = Effect.fn("Agent.renewResume")(function* (scope: AgentScope, claim: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(agentInterrupts).set({ claimedAt: new Date() }).where(and(owned(scope), eq(agentInterrupts.claim, claim)));
});

/** Ends `claim`'s hold on the interrupts it resumed, answered or not. */
export const releaseResume = Effect.fn("Agent.releaseResume")(function* (scope: AgentScope, claim: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(agentInterrupts).set({ claim: null, claimedAt: null }).where(and(owned(scope), eq(agentInterrupts.claim, claim)));
});
