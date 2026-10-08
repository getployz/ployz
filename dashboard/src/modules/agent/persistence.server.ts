import "@tanstack/react-start/server-only";
import type { ModelMessage, RunRecord, RunStore } from "@tanstack/ai";
import type { ChatWithInterruptsPersistence, InterruptRecord, InterruptStore, MessageStore } from "@tanstack/ai-persistence";
import { and, asc, desc, eq, inArray, type SQL, sql } from "drizzle-orm";
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

/** TanStack AI's messages, runs and interrupts stores over Postgres, confined to `scope`. */
export const agentPersistence = Effect.fn("Agent.persistence")(function* (scope: AgentScope) {
  const run = Effect.runPromiseWith(yield* Effect.context<Database>());
  const database = yield* Database;
  const { drizzle } = database;
  const threadOwned = and(eq(agentThreads.organizationId, scope.organizationId), eq(agentThreads.userId, scope.userId));
  const runOwned = and(eq(agentRuns.organizationId, scope.organizationId), eq(agentRuns.userId, scope.userId));
  const interruptOwned = and(eq(agentInterrupts.organizationId, scope.organizationId), eq(agentInterrupts.userId, scope.userId));

  const messages: MessageStore = {
    loadThread: async (threadId: string) => {
      const [row] = await run(drizzle.select({ messages: agentThreads.messages }).from(agentThreads)
        .where(and(threadOwned, eq(agentThreads.threadId, threadId))));
      return row?.messages ?? [];
    },
    saveThread: async (threadId: string, saved: Array<ModelMessage>) => {
      const [row] = await run(drizzle.insert(agentThreads).values({ threadId, ...scope, messages: saved })
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
      await run(drizzle.insert(agentRuns).values({ ...scope, runId: record.runId, threadId: record.threadId, status: record.status, startedAt: record.startedAt, record })
        .onConflictDoNothing());
      const existing = await getRun(input.runId);
      if (existing === null) throw new Error(`Run ${input.runId} belongs to another member.`);
      return existing;
    },
    update: async (runId, patch) => {
      const set: PgUpdateSetSource<typeof agentRuns> = { record: merged(agentRuns.record, patch), updatedAt: new Date() };
      if (patch.status !== undefined) set.status = patch.status;
      await run(drizzle.update(agentRuns).set(set).where(and(runOwned, eq(agentRuns.runId, runId))));
    },
    get: getRun,
    findActiveRun: async (threadId) => {
      const [row] = await run(drizzle.select({ record: agentRuns.record }).from(agentRuns)
        .where(and(runOwned, eq(agentRuns.threadId, threadId), eq(agentRuns.status, "running")))
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
      await run(drizzle.insert(agentInterrupts).values({
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
      await run(settle(interruptId, { status: "resolved", resolvedAt: Date.now(), response }));
    },
    cancel: async (interruptId) => {
      await run(settle(interruptId, { status: "cancelled", resolvedAt: Date.now() }));
    },
    commitBatch: (entries) => run(database.transaction(Effect.gen(function* () {
      const ids = entries.map((entry) => entry.interruptId);
      if (new Set(ids).size !== ids.length) return yield* Effect.die(new Error("An interrupt batch names one interrupt twice."));
      const found = yield* drizzle.select({ id: agentInterrupts.interruptId }).from(agentInterrupts)
        .where(and(interruptOwned, pending, inArray(agentInterrupts.interruptId, ids))).for("update");
      if (found.length !== ids.length) return yield* Effect.die(new Error("An interrupt batch names an interrupt that is not pending."));
      const resolvedAt = Date.now();
      for (const entry of entries) {
        yield* settle(entry.interruptId, entry.status === "resolved"
          ? { status: "resolved", resolvedAt, response: entry.response }
          : { status: "cancelled", resolvedAt });
      }
    }))),
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

/** How long a resuming run holds its interrupts before another client may take them over: a run that died mid-turn. */
const CLAIM_LEASE_MS = 5 * 60 * 1000;

/**
 * Whether `runId` may answer `interruptIds`: `claimed` once it holds every pending one, `busy` while another run holds
 * one, `settled` once another run answered them all, and `unchecked` when one is missing so the chat rejects the resume.
 */
export const claimResume = Effect.fn("Agent.claimResume")(function* (scope: AgentScope, runId: string, interruptIds: ReadonlyArray<string>) {
  const database = yield* Database;
  const { drizzle } = database;
  const owned = and(eq(agentInterrupts.organizationId, scope.organizationId), eq(agentInterrupts.userId, scope.userId));
  return yield* database.transaction(Effect.gen(function* () {
    const ids = [...new Set(interruptIds)];
    const rows = yield* drizzle.select({ status: agentInterrupts.status, claimedByRunId: agentInterrupts.claimedByRunId, claimedAt: agentInterrupts.claimedAt })
      .from(agentInterrupts).where(and(owned, inArray(agentInterrupts.interruptId, ids))).for("update");
    if (rows.length < ids.length) return "unchecked" as const;
    const leased = new Date(Date.now() - CLAIM_LEASE_MS);
    if (rows.some((row) => row.claimedByRunId !== null && row.claimedByRunId !== runId && row.claimedAt !== null && row.claimedAt > leased)) return "busy" as const;
    if (rows.every((row) => row.status !== "pending")) return "settled" as const;
    yield* drizzle.update(agentInterrupts).set({ claimedByRunId: runId, claimedAt: new Date() })
      .where(and(owned, inArray(agentInterrupts.interruptId, ids), eq(agentInterrupts.status, "pending")));
    return "claimed" as const;
  }));
});

/** Ends `runId`'s hold on the interrupts it resumed, answered or not. */
export const releaseResume = Effect.fn("Agent.releaseResume")(function* (scope: AgentScope, runId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(agentInterrupts).set({ claimedByRunId: null, claimedAt: null }).where(and(
    eq(agentInterrupts.organizationId, scope.organizationId),
    eq(agentInterrupts.userId, scope.userId),
    eq(agentInterrupts.claimedByRunId, runId),
  ));
});
