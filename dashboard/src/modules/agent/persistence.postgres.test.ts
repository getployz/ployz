import type { ModelMessage } from "@tanstack/ai";
import { runPersistenceConformance } from "@tanstack/ai-persistence/testkit";
import { afterAll, beforeAll, expect, it } from "vitest";
import { type AgentScope, agentPersistence, threadAvailable } from "#/modules/agent/persistence.server";
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
