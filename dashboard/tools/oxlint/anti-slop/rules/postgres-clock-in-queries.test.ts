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
      code: `const unheld = sql\`(\${serverAccess.claim} is null or \${serverAccess.updatedAt} < \${checkedNow()} - make_interval(secs => 90))\`;
      const leased = yield* drizzle.update(serverAccess).set({ claim: token, updatedAt: sql\`now()\` })
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
    // A written time stays now(), with or without an interval, and an \`=\` in what .set, .values or an upsert's set
    // writes assigns.
    {
      filename,
      code: `yield* drizzle.insert(verification).values({ identifier, value, expiresAt: sql\`now() + make_interval(secs => \${MAX_AGE_SECONDS})\` });
      yield* drizzle.update(serviceAct).set({ endedAt: sql\`now()\` }).where(eq(serviceAct.id, id));
      const age = sql<number>\`extract(epoch from now() - \${serviceAct.startedAt})\`;
      const stamp = sql\`case when \${serviceAct.state} = 'running' then now() end\`;
      yield* drizzle.update(serviceAct).set({ endedAt: stamp });
      yield* drizzle.insert(serviceAct).values([{ id, endedAt: sql\`case when \${state} = 'done' then now() end\` }])
        .onConflictDoUpdate({ target: serviceAct.id, set: { endedAt: sql\`case when excluded.state = 'done' then now() end\` } });`,
    },
    // A check reads checkedNow(); a sweep that needs its time index reads sweepCutoff().
    {
      filename,
      code: `import { and, eq, gt, lt } from "drizzle-orm";
      const live = sql<boolean>\`\${verification.expiresAt} > \${checkedNow()}\`;
      await drizzle.select().from(invitation).where(and(eq(invitation.id, id), gt(invitation.expiresAt, checkedNow())));
      await drizzle.delete(rateLimitEvent).where(lt(rateLimitEvent.at, sql\`\${sweepCutoff()} - make_interval(secs => 86400)\`));`,
    },
    // A lock by id, then the time check in the next statement; a plain range read and a prune on sweepCutoff().
    {
      filename,
      code: `import { and, eq, gt, lt } from "drizzle-orm";
      await drizzle.select({ id: fleetToken.id }).from(fleetToken).where(eq(fleetToken.id, id)).for("no key update");
      const [live] = await drizzle.select({ id: fleetToken.id }).from(fleetToken).where(and(eq(fleetToken.id, id), gt(fleetToken.expiresAt, checkedNow())));
      await drizzle.update(fleetToken).set({ usedAt: sql\`now()\` }).where(and(eq(fleetToken.id, id), gt(fleetToken.expiresAt, checkedNow())));
      const recent = await drizzle.select().from(rateLimitEvent).where(gt(rateLimitEvent.at, sql\`\${sweepCutoff()} - make_interval(secs => 60)\`));
      await database.drizzle.delete(rateLimitEvent).where(lt(rateLimitEvent.at, sql\`\${sweepCutoff()} - make_interval(secs => 86400)\`)).pipe(Effect.ignore);`,
    },
    { filename: "feature.postgres.test.ts", code: `yield* drizzle.update(session).set({ expiresAt: new Date(Date.now() - 1_000) });` },
  ],
  invalid: [
    // The sandbox claim before round 3: it waited on the Sandbox's row lock past the binding's deadline, then still
    // removed the sign-in's Organization, since now() is when its transaction began.
    {
      filename,
      code: `const bindings = yield* drizzle.delete(verification)
        .where(eq(verification.identifier, bindingIdentifier(userId, hashClaimSecret(secret))))
        .returning({ organizationId: verification.value, live: sql<boolean>\`\${verification.expiresAt} > now()\` });`,
      errors: [{ messageId: "staleNow" }],
    },
    // Invite acceptance before round 2: the accept read the expiry with the invite row it waited to lock.
    {
      filename,
      code: `import { and, eq, gt } from "drizzle-orm";
      const expired = sql<boolean>\`\${invitation.expiresAt} <= now()\`;
      const [row] = await drizzle.select({ id: invitation.id, expired }).from(invitation).where(byToken(token)).for("update");
      await drizzle.select({ id: invitation.id }).from(invitation).where(and(eq(invitation.status, "pending"), gt(invitation.expiresAt, sql\`now()\`)));`,
      errors: [{ messageId: "staleNow" }, { messageId: "staleNow" }],
    },
    // Through a const, a where, a having, an upsert's setWhere and a nested fragment.
    {
      filename,
      code: `const cutoff = sql\`now() - make_interval(secs => 90)\`;
      yield* drizzle.update(serviceAct).set({ state: "unknown" }).where(sql\`\${serviceAct.startedAt} < \${cutoff}\`);
      yield* drizzle.select().from(serviceAct).where(sql\`\${serviceAct.endedAt} is null and now() is not null\`);
      yield* drizzle.select().from(serviceAct).groupBy(serviceAct.id).having(sql\`max(\${serviceAct.startedAt}) < now()\`);
      yield* drizzle.insert(serverAccess).values(row).onConflictDoUpdate({ target, set: { updatedAt: sql\`now()\` }, setWhere: sql\`\${serverAccess.claim} is null or \${serverAccess.updatedAt} < now() - make_interval(secs => 90)\` });`,
      errors: Array.from({ length: 4 }, () => ({ messageId: "staleNow" })),
    },
    // A raw clock in a check names neither helper, so nothing says which clock it meant.
    {
      filename,
      code: `export const tokenRuns = sql\`\${organizationToken.expiresAt} > clock_timestamp()\`;
      export const ended = sql\`\${organizationToken.expiresAt} <= (select clock_timestamp())\`;`,
      errors: [{ messageId: "rawClock" }, { messageId: "rawClock" }],
    },
    // Through two const aliases.
    {
      filename,
      code: `import { lt } from "drizzle-orm";
      const cutoff = sql\`now() - interval '1 minute'\`;
      const limit = cutoff;
      await drizzle.delete(change).where(lt(change.createdAt, limit));`,
      errors: [{ messageId: "staleNow" }],
    },
    // A predicate built with sql.raw, directly or through a const string.
    {
      filename,
      code: `import { lt } from "drizzle-orm";
      const raw = "expires_at > now()";
      await drizzle.select().from(session).where(sql.raw("expires_at > now()"));
      await drizzle.select().from(session).where(sql.raw(raw));
      await drizzle.select().from(session).where(lt(session.expiresAt, sql.raw(\`clock_timestamp()\`)));
      const live = sql.raw("expires_at > current_timestamp");`,
      errors: [{ messageId: "staleNow" }, { messageId: "staleNow" }, { messageId: "rawClock" }, { messageId: "staleNow" }],
    },
    // Whitespace inside the call.
    {
      filename,
      code: `yield* drizzle.select().from(session).where(sql\`\${session.expiresAt} > now ()\`);
      const live = sql\`\${session.expiresAt} > clock_timestamp ( )\`;`,
      errors: [{ messageId: "staleNow" }, { messageId: "rawClock" }],
    },
    // An \`=\` outside what a write assigns compares: a returned flag, a select, and a raw statement.
    {
      filename,
      code: `yield* drizzle.update(session).set({ seen: true }).returning({ due: sql<boolean>\`\${session.expiresAt} = now()\` });
      yield* drizzle.select({ due: sql<boolean>\`\${session.expiresAt} = clock_timestamp()\` }).from(session);
      const edit = sql\`update uptime_check set revision = revision + 1, updated_at = now() where id = \${id}\`;`,
      errors: [{ messageId: "staleNow" }, { messageId: "rawClock" }, { messageId: "staleNow" }],
    },
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
    // The Fleet Token arrival before round 6: a lock-only holder let an expired token through, since a locking read
    // checks before it waits. Also through a const, a projection, and a query a const holds.
    {
      filename,
      code: `import { and, eq, gt } from "drizzle-orm";
      const [live] = await drizzle.select({ max: fleetToken.autoEnrollMax }).from(fleetToken)
        .where(and(eq(fleetToken.id, id), gt(fleetToken.expiresAt, checkedNow()))).for("no key update");
      const expired = sql<boolean>\`\${invitation.expiresAt} <= \${checkedNow()}\`;
      const [row] = await drizzle.select({ id: invitation.id, expired }).from(invitation).where(byToken(token)).for("update").limit(1);
      const query = drizzle.select({ left: sql<number>\`\${drain.deadline} - \${checkedNow()}\` }).from(drain).where(eq(drain.id, id));
      const [held] = lock ? await query.for("update") : await query;
      await drizzle.execute(sql\`select id from session where expires_at > \${checkedNow()} for share\`);`,
      errors: Array.from({ length: 4 }, () => ({ messageId: "lockedCheck" })),
    },
    // sweepCutoff() where a statement locks, updates, or returns the rows it deletes.
    {
      filename,
      code: `import { gt, lt } from "drizzle-orm";
      await drizzle.select({ id: invite.id }).from(invite).where(gt(invite.expiresAt, sweepCutoff())).for("update");
      await drizzle.update(session).set({ revoked: true }).where(lt(session.expiresAt, sweepCutoff()));
      const gone = await drizzle.delete(claim).where(lt(claim.expiresAt, sweepCutoff())).returning({ id: claim.id });
      await drizzle.insert(lease).values(row).onConflictDoUpdate({ target: lease.id, set: { holder }, setWhere: lt(lease.renewedAt, sweepCutoff()) });
      await drizzle.execute(sql\`update lease set holder = null where renewed_at < \${sweepCutoff()}\`);`,
      errors: Array.from({ length: 5 }, () => ({ messageId: "sweepActs" })),
    },
  ],
});
