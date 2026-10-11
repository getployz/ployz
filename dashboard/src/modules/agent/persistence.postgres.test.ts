import type { ModelMessage } from "@tanstack/ai";
import { runPersistenceConformance } from "@tanstack/ai-persistence/testkit";
import { eq } from "drizzle-orm";
import { afterAll, beforeAll, expect, it } from "vitest";
import { type AgentScope, agentPersistence, claimResume, startRun, threadAvailable } from "#/modules/agent/persistence.server";
import { agentRuns } from "#/modules/agent/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

let harness: PostgresTestHarness;
let ada: AgentScope;
let bob: AgentScope;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const [org] = await harness.db.insert(organization).values({ name: "Shop", slug: "shop" }).returning();
  const members = await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
  ]).returning();
  const organizationId = org?.id ?? "";
  ada = { organizationId, userId: members[0]?.id ?? "" };
  bob = { organizationId, userId: members[1]?.id ?? "" };
});
afterAll(() => harness.stop());

runPersistenceConformance("Postgres agent sidebar", () => harness.runEffect(agentPersistence(ada)), {
  skip: ["activities", "metadata", "generationRuns", "artifacts", "blobs"],
  checks: ["messages.metadata", "runs.listByThread.state"],
});

it("keeps one member's thread, runs and interrupts out of another member's reach", async () => {
  const mine = await harness.runEffect(agentPersistence(ada));
  const theirs = await harness.runEffect(agentPersistence(bob));
  const hello: ModelMessage[] = [{ role: "user", content: "list services" }];
  await mine.stores.messages.saveThread("thread-ada", hello);
  await mine.stores.runs.createOrResume({ runId: "run-ada", threadId: "thread-ada", startedAt: 1 });
  await mine.stores.interrupts.create({ interruptId: "ask-ada", runId: "run-ada", threadId: "thread-ada", requestedAt: 1, payload: {} });

  expect(await theirs.stores.messages.loadThread("thread-ada")).toEqual([]);
  await expect(theirs.stores.messages.saveThread("thread-ada", [])).rejects.toThrow("belongs to another member");
  await expect(theirs.stores.runs.createOrResume({ runId: "run-ada", threadId: "thread-ada", startedAt: 2 }))
    .rejects.toThrow("belongs to another member");
  expect(await theirs.stores.runs.findActiveRun("thread-ada")).toBeNull();
  expect(await theirs.stores.interrupts.listPending("thread-ada")).toEqual([]);
  await theirs.stores.interrupts.cancel("ask-ada");

  expect(await mine.stores.messages.loadThread("thread-ada")).toEqual(hello);
  expect(await mine.stores.interrupts.get("ask-ada")).toMatchObject({ status: "pending" });
  expect(await harness.runEffect(threadAvailable(ada, "thread-ada"))).toBe(true);
  expect(await harness.runEffect(threadAvailable(bob, "thread-ada"))).toBe(false);
  expect(await harness.runEffect(threadAvailable(bob, "thread-new"))).toBe(true);
});

it("a claim never reopens its own aborted or ordinarily failed run, nor a superseded-coded run no takeover displaced", async () => {
  const plain = await harness.runEffect(agentPersistence(ada));
  await plain.stores.interrupts.create({ interruptId: "ask-own", runId: "run-asking", threadId: "thread-own", requestedAt: 1, payload: {} });
  expect(await harness.runEffect(claimResume(ada, "X", ["ask-own"]))).toBe("claimed");
  const x = await harness.runEffect(agentPersistence(ada, "X"));
  await x.stores.runs.createOrResume({ runId: "own-aborted", threadId: "thread-own", startedAt: 1 });
  await x.stores.runs.update("own-aborted", { status: "aborted", finishedAt: 11 });
  await x.stores.runs.createOrResume({ runId: "own-failed", threadId: "thread-own", startedAt: 1 });
  await x.stores.runs.update("own-failed", { status: "failed", finishedAt: 12, error: { message: "boom", code: "overloaded" } });
  await plain.stores.runs.createOrResume({ runId: "lookalike", threadId: "thread-own", startedAt: 1 });
  await plain.stores.runs.update("lookalike", { status: "failed", finishedAt: 13, error: { message: "m", code: "superseded" } });

  expect(await x.stores.runs.createOrResume({ runId: "own-aborted", threadId: "thread-own", startedAt: 2 }))
    .toMatchObject({ status: "aborted", finishedAt: 11 });
  expect(await x.stores.runs.createOrResume({ runId: "own-failed", threadId: "thread-own", startedAt: 2 }))
    .toMatchObject({ status: "failed", finishedAt: 12, error: { message: "boom" } });
  expect(await x.stores.runs.createOrResume({ runId: "lookalike", threadId: "thread-own", startedAt: 2 }))
    .toMatchObject({ status: "failed", finishedAt: 13, error: { code: "superseded" } });
  const [lookalike] = await harness.db.select({ claim: agentRuns.claim }).from(agentRuns).where(eq(agentRuns.runId, "lookalike"));
  expect(lookalike).toEqual({ claim: null });
});

it("a run another member or Organization took is foreign to the caller, and the caller's own retry sees it running", async () => {
  const [elsewhere] = await harness.db.insert(organization).values({ name: "Other", slug: "other" }).returning();
  const adaElsewhere = { organizationId: elsewhere?.id ?? "", userId: ada.userId };

  expect(await harness.runEffect(startRun(ada, "thread-run", "run-taken"))).toBe("started");
  expect(await harness.runEffect(startRun(ada, "thread-run", "run-taken"))).toBe("running");
  expect(await harness.runEffect(startRun(ada, "thread-elsewhere", "run-taken"))).toBe("foreign");
  expect(await harness.runEffect(startRun(bob, "thread-run", "run-taken"))).toBe("foreign");
  expect(await harness.runEffect(startRun(adaElsewhere, "thread-run", "run-taken"))).toBe("foreign");
});
