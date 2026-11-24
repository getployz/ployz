import { RuleTester } from "oxlint/plugins-dev";
import { describe, it } from "vitest";

import { postgresClockInQueriesRule } from "./postgres-clock-in-queries.ts";

RuleTester.describe = describe;
RuleTester.it = it;

const filename = "feature.server.ts";
const tester = new RuleTester();

tester.run("postgres-clock-in-queries", postgresClockInQueriesRule, {
  valid: [
    // 98aab905bb, fixed.
    {
      filename,
      code: `yield* drizzle.update(machineEnrollmentToken).set({ expiresAt: sql\`now()\` }).where(and(
        inArray(machineEnrollmentToken.id, cancelling.map((row) => row.tokenId)),
        isNull(machineEnrollmentToken.joinedMachineId),
      ));`,
    },
    // 03981fa91f, fixed.
    {
      filename,
      code: `const now = sql\`clock_timestamp()\`;
      const unheld = sql\`(\${serverAccess.claim} is null or \${serverAccess.updatedAt} < \${now} - make_interval(secs => 90))\`;
      const leased = yield* drizzle.update(serverAccess).set({ claim: token, updatedAt: now })
        .where(and(holder, isNotNull(serverAccess.revokedAt), unheld)).returning({ machineId: serverAccess.machineId });`,
    },
    {
      filename,
      code: `const run = { status: "failed", finishedAt: Date.now() };
      yield* drizzle.update(agentRuns).set({ record: merged(agentRuns.record, { finishedAt: Date.now() }), updatedAt: sql\`now()\` });
      yield* drizzle.insert(agentRuns).values({ runId, record: run });
      cache.set(key, new Date());`,
    },
    {
      filename,
      code: `const gt = (left: number, right: number) => left > right;
      const fresh = gt(Date.now(), expiresAt);
      const row = { at: new Date() };
      function later() { return drizzle.insert(log).values(row); }`,
    },
    { filename: "feature.postgres.test.ts", code: `yield* drizzle.update(session).set({ expiresAt: new Date(Date.now() - 1_000) });` },
  ],
  invalid: [
    {
      filename,
      code: `import { and, gt } from "drizzle-orm";
      const leased = gt(agentInterrupts.claimedAt, new Date(Date.now() - CLAIM_LEASE_MS));
      const rows = await drizzle.select().from(agentInterrupts).where(and(owned, leased));`,
      errors: [{ messageId: "webClock" }],
    },
    {
      filename,
      code: `const set = { record: merged(agentRuns.record, patch), updatedAt: new Date() };
      if (patch.status !== undefined) set.status = patch.status;
      await drizzle.update(agentRuns).set(set).where(eq(agentRuns.runId, runId));`,
      errors: [{ messageId: "webClock" }],
    },
    // The cleanup row's waiting write, through a spread and an upsert's set.
    {
      filename,
      code: `const waiting = { runId, state: "waiting", updatedAt: new Date() };
      yield* drizzle.insert(deploymentCleanup).values({ ...cleanupOf(data), ...waiting })
        .onConflictDoUpdate({ target: deploymentCleanup.deploymentId, set: waiting });`,
      errors: [{ messageId: "webClock" }],
    },
    // 98aab905bb: a cancelled enrollment's token expired on the web instance's clock while the claim reads Postgres's.
    {
      filename,
      code: `yield* drizzle.update(machineEnrollmentToken).set({ expiresAt: new Date() }).where(and(
        inArray(machineEnrollmentToken.id, cancelling.map((row) => row.tokenId)),
        isNull(machineEnrollmentToken.joinedMachineId),
      ));`,
      errors: [{ messageId: "webClock" }],
    },
    // 03981fa91f: the Server access lease wrote and compared one instance's clock against another's.
    {
      filename,
      code: `const unheld = (now: Date) =>
        sql\`(\${serverAccess.claim} is null or \${serverAccess.updatedAt} < \${new Date(now.getTime() - CLAIM_LEASE_MS)})\`;
      const now = new Date();
      const claimed = yield* drizzle.insert(serverAccess).values({ machineId, claim: token }).onConflictDoUpdate({
        target: [serverAccess.organizationId, serverAccess.credentialId, serverAccess.machineId],
        set: { claim: token, updatedAt: now },
        setWhere: sql\`\${serverAccess.encryptedCapability} is null and \${unheld(now)}\`,
      });
      const leased = yield* drizzle.update(serverAccess).set({ claim: token, updatedAt: now })
        .where(and(holder, isNotNull(serverAccess.revokedAt), unheld(now)));
      yield* tx.update(serverAccess).set({ revokedAt: new Date(), encryptedCapability: null, claim: null, updatedAt: new Date() });`,
      errors: Array.from({ length: 6 }, () => ({ messageId: "webClock" })),
    },
    {
      filename,
      code: `const now = Date.now();
      yield* drizzle.update(serviceAct).set({ state: "unknown" })
        .where(and(eq(serviceAct.state, "running"), lt(serviceAct.startedAt, new Date(now - SERVICE_ACT_RUNNING_LIMIT_MS))));`,
      errors: [{ messageId: "webClock" }],
    },
    // A typed patch: \`satisfies\` and \`as\` wrap the object the write takes.
    {
      filename,
      code: `const patch = { updatedAt: new Date() } satisfies Patch;
      yield* drizzle.update(serviceAct).set(patch).where(eq(serviceAct.id, id));
      yield* drizzle.update(serviceAct).set({ endedAt: new Date() } as Patch).where(eq(serviceAct.id, id));
      yield* drizzle.update(serviceAct).set({ startedAt: new Date() satisfies Date }).where(eq(serviceAct.id, id));`,
      errors: Array.from({ length: 3 }, () => ({ messageId: "webClock" })),
    },
  ],
});
