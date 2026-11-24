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
    { filename: "feature.postgres.test.ts", code: `yield* drizzle.update(session).set({ expiresAt: new Date(Date.now() - 1_000) });` },
  ],
  invalid: [
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
  ],
});
