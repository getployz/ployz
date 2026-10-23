import "@tanstack/react-start/server-only";
import { and, eq, isNotNull, isNull, not, sql, type SQLWrapper } from "drizzle-orm";
import { Cause, Clock, Effect, Schema } from "effect";
import type { DialTiming } from "@ployz/sdk";
import type { MachineId } from "#/db/tables";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { type Caller, callerCan } from "#/modules/identity/actor";
import { member, organizationToken, session } from "#/modules/identity/tables";
import { decryptPairingSecret, loadOrganizationConnections } from "#/modules/machines/connections.server";
import { dialCorrelation, dialTimingFields, nativeDialTimingFields, sampleDialTiming, shouldLogDialTiming, type DialTimingInput } from "#/modules/machines/server-access-diagnostics";
import { lacks } from "#/modules/organization/access.server";
import { enrollmentAllocation, organizationMachine, serverAccess } from "#/modules/machines/tables";
import { clusterRead, SESSION_CLOSE_CLUSTER_MS, withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import { type PloyzSdkError, Ployz, RpcError, rpcErrorMessage, sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

/** One dial or Clear per Server; a Server that doesn't answer in time is unreachable or unconfirmed. */
const SERVER_TIMEOUT_MS = 10_000;
const SERVER_CONCURRENCY = 4;
/** The column's check admits any array, so an assignment without a text name is left out of `names`. */
const isNamed = Schema.is(Schema.Struct({ machine: Schema.Struct({ id: Schema.String, name: Schema.String }) }));

/** A credential's Management Client label on every Server: `cli-` and the first 28 hex digits of its id. */
export function serverAccessLabel(credentialId: string) {
  return `cli-${credentialId.replaceAll("-", "").slice(0, 28)}`;
}

/** Provisioning and revoking one credential's holders take turns, so a revocation never misses a fresh holder. */
const CREDENTIAL_LOCK = 1_349_603;

const lockCredential = (credentialId: string, mode: "exclusive" | "shared") => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(mode === "shared"
    ? sql`select pg_advisory_xact_lock_shared(${CREDENTIAL_LOCK}, hashtext(${credentialId}))`
    : sql`select pg_advisory_xact_lock(${CREDENTIAL_LOCK}, hashtext(${credentialId}))`);
});

/**
 * A Server keeps one pending key per slot, so one Set at a time per credential and Server: a first use claims the
 * holder's row before its Set, with no key yet, and another first use in flight waits for the key the claimant
 * stores. The claim is a row, not a lock, so no connection is held while the Server answers, and a Server that hangs
 * holds up nobody else. A revoked row owes its Server a Clear, which a first use that finds one runs itself unless
 * a sweep is already running it; either way nothing Sets the slot until that Clear is answered. A Set or Clear in
 * flight is named on the row by the request that runs it, for as long as that request waits on it: a call that
 * timed out gives up its name while its request may still reach the Server. A claimant that stalls past the lease
 * is taken over, and the name fences it: its store or delete then touches nothing a successor took. The token orders
 * what Cloud stores, not what reaches the Server: the daemon finishes work it admitted, so a Set or Clear whose
 * caller timed out can land after a successor's, as on main. A Clear's delete takes only a row still revoked: a key
 * provisioned over the row meanwhile, as an instance from before the claim column does without touching it, is a
 * live holder again until its credential loses authorization and the next sweep Clears it.
 */
const CLAIM_LEASE_MS = SERVER_TIMEOUT_MS * 3;
const CLAIM_POLL = "200 millis";
/** Timestamps the lease compares come from one clock, the database's, whatever instance wrote them. */
const now = sql`clock_timestamp()`;
/** Nobody acts on the row's slot now, or whoever did has outlived the lease. */
const unheld = sql`(${serverAccess.claim} is null
  or ${serverAccess.updatedAt} < ${now} - make_interval(secs => ${sql.raw(String(CLAIM_LEASE_MS / 1000))}))`;

/** What a Server gave one holder: its key, or what it said instead, or neither when nothing came in time. */
type Provisioned = { readonly capability: string | null; readonly refused: string | null; readonly revoked: boolean };

/** `value`, or why the Server gave none. */
const answered = <A, R>(call: Effect.Effect<A, PloyzSdkError | Cause.TimeoutError, R>) => call.pipe(
  Effect.map((value) => ({ value, refused: null, outcome: "answered" as const })),
  Effect.catch((error) => Effect.succeed({ value: null, refused: refusal(error), outcome: dialOutcome(error) })),
);

function dialOutcome(error: PloyzSdkError | Cause.TimeoutError): DialTimingInput["outcome"] {
  if (Cause.isTimeoutError(error)) return "timed_out";
  if ("cause" in error && error.cause instanceof RpcError) {
    if (error.cause.dialTiming?.outcome === "timed_out") return "timed_out";
    if (error.cause.unanswered) return error.cause.message === "connection cancelled" ? "cancelled" : "unanswered";
    return "refused";
  }
  return "refused";
}

const logDialTiming = (input: DialTimingInput) => {
  if (!shouldLogDialTiming(input)) return Effect.void;
  const fields = dialTimingFields(input);
  const log = input.outcome !== "answered" && input.stage === "cloud_dial"
    ? Effect.logInfo : input.outcome !== "answered" ? Effect.logWarning
      : input.totalMs >= 2_000 ? Effect.logInfo : Effect.logDebug;
  return log("Server Access dial timing").pipe(Effect.annotateLogs(fields));
};

const logNativeDialTiming = (timing: DialTiming, cloudOutcome: DialTimingInput["outcome"]) => {
  const log = timing.outcome !== "answered" ? Effect.logWarning
    : timing.totalMs >= 2_000 ? Effect.logInfo : Effect.logDebug;
  return log("Server Access native dial timing").pipe(Effect.annotateLogs(nativeDialTimingFields(timing, cloudOutcome)));
};

/**
 * Why a Server gave no holder, or null when it sent no reply: Cloud's timeout ran out, or the SDK says the dial failed
 * or the call got no status back. Only those are unreachable; a Server that refused may well be running.
 */
function refusal(error: PloyzSdkError | Cause.TimeoutError) {
  if (Cause.isTimeoutError(error)) return null;
  if ("cause" in error && error.cause instanceof RpcError && error.cause.unanswered) return null;
  return rpcErrorMessage(error) ?? sdkFailureMessage(error);
}

/** Only `ployz` credentials hold a Server slot; a remote agent reaches Servers through Cloud. */
type ServerAccessCaller = Caller & { readonly credential: { readonly kind: "session" | "token" } };

/**
 * The Caller's own capability on every Server of its Organization, provisioning a missing `cli-<id>` holder on
 * first use, including on Servers enrolled after login. A Server that doesn't answer now is listed as unreachable, and
 * one that refused, with what it said.
 * Only an owner or admin gets one: a holder is full control of the Server.
 */
export const provideServerAccess = Effect.fn("ServerAccess.provide")(function* (caller: ServerAccessCaller) {
  const lacking = lacks({ ...caller, support: null }, "servers:shell");
  if (lacking !== null) {
    const slug = caller.organization.slug;
    return { ok: false as const, refusal: {
      code: "forbidden",
      message: callerCan({ role: caller.role }, "servers:shell") ? lacking : `Server access needs an admin. Ask an owner or admin of ${slug}.`,
      details: { organization: slug },
    } satisfies StoreRefusal };
  }
  const loaded = yield* loadOrganizationConnections(caller.organization.id);
  if (loaded.kind === "missing") return { ok: true as const, value: { connections: [], unreachable: [], refused: [], names: {} } };
  const database = yield* Database;
  const { drizzle } = database;
  const encryption = yield* SecretEncryption;
  const held = new Map((yield* drizzle.select({
    machineId: serverAccess.machineId,
    encryptedCapability: serverAccess.encryptedCapability,
  }).from(serverAccess).where(and(
    eq(serverAccess.organizationId, caller.organization.id),
    eq(serverAccess.credentialId, caller.credential.id),
    isNull(serverAccess.revokedAt),
  ))).map((row) => [row.machineId, row.encryptedCapability]));
  const label = serverAccessLabel(caller.credential.id);
  // Every Server at once, so the Cluster time is one dial and Set plus one close (12 s) inside the CLI's 15 s request.
  // One waiting for another's Set waits until its own deadline at most.
  const results = yield* Effect.forEach(loaded.connections, (cloud) => Effect.gen(function* () {
    const machineId = cloud.machine_id;
    if (machineId === undefined) return yield* Effect.die("an Organization connection names its Machine");
    const stored = held.get(machineId);
    if (stored != null) return { machine_id: machineId, management: yield* decryptPairingSecret(stored), refused: null };
    const holder = and(
      eq(serverAccess.organizationId, caller.organization.id),
      eq(serverAccess.credentialId, caller.credential.id),
      eq(serverAccess.machineId, machineId),
    );
    // Dial, claim, Set, then in one short transaction store the key and check the credential. A first use that
    // found the row claimed waits for the key the claimant stores, so a second Set never replaces the first's key on
    // the Server. A claimant that lost its claim meanwhile stores nothing and takes what the row says instead.
    // A revocation that finished meanwhile revokes the stored key at once, so the Clear below (or the next retry)
    // takes it off the Server. Not interrupted: a cancelled statement would cancel the connection's next.
    const provisioned: Provisioned = yield* Effect.scoped(Effect.gen(function* () {
      const ployz = yield* Ployz;
      const runLog = Effect.runForkWith(yield* Effect.context());
      const deadline = (yield* Clock.currentTimeMillis) + SERVER_TIMEOUT_MS;
      const sampled = sampleDialTiming();
      const correlation = dialCorrelation();
      let cloudDialOutcome: DialTimingInput["outcome"] | undefined;
      const pendingNativeTimings: DialTiming[] = [];
      const onDialTiming = (timing: DialTiming) => {
        if (cloudDialOutcome === undefined) pendingNativeTimings.push(timing);
        else runLog(logNativeDialTiming(timing, cloudDialOutcome));
      };
      const dialStarted = performance.now();
      const dialed = yield* answered(ployz.connect({
        connections: [cloud], timeoutMs: SERVER_TIMEOUT_MS, sampleDialTiming: sampled, dialCorrelation: correlation,
        onDialTiming,
      }, { closeWithin: clusterRead })
        .pipe(Effect.timeout(SERVER_TIMEOUT_MS)));
      const dialMs = performance.now() - dialStarted;
      cloudDialOutcome = dialed.outcome;
      for (const timing of pendingNativeTimings) runLog(logNativeDialTiming(timing, cloudDialOutcome));
      yield* logDialTiming({ stage: "cloud_dial", totalMs: dialMs, outcome: dialed.outcome, sampled, correlation });
      if (dialed.value === null) return { capability: null, refused: dialed.refused, revoked: false };
      const client = dialed.value;
      const timeLeft = Clock.currentTimeMillis.pipe(Effect.map((now) => Math.max(1, deadline - now)));
      const token = crypto.randomUUID();
      const holding = eq(serverAccess.claim, token);
      let reclaiming = false;
      let landed = false;
      while (true) {
        // The row this request held went from under it, as a revocation does, or this request Cleared a revoked row
        // itself: no second Set for a credential that no longer authorizes one. A Set this request landed is
        // recorded revoked over whatever the row holds, a Clear's token included, since that Clear may have emptied
        // the slot before the Set landed; retireServerAccess below, or the sweep, Clears it again.
        if (reclaiming) {
          const [checked] = yield* drizzle.select({
            authorized: sql<boolean>`${authorizedBy(caller.userId, caller.organization.id, caller.credential.kind, caller.credential.id)}`,
          }).from(member).where(and(eq(member.userId, caller.userId), eq(member.organizationId, caller.organization.id)));
          if (checked?.authorized !== true) {
            if (landed) {
              yield* drizzle.insert(serverAccess).values({
                organizationId: caller.organization.id,
                machineId,
                credentialId: caller.credential.id,
                credentialKind: caller.credential.kind,
                userId: caller.userId,
                revokedAt: now,
                updatedAt: now,
              }).onConflictDoUpdate({
                target: [serverAccess.organizationId, serverAccess.credentialId, serverAccess.machineId],
                set: { revokedAt: now, encryptedCapability: null, claim: null, updatedAt: now },
              });
            }
            return { capability: null, refused: null, revoked: true };
          }
        }
        const claimed = yield* drizzle.insert(serverAccess).values({
          organizationId: caller.organization.id,
          machineId,
          credentialId: caller.credential.id,
          credentialKind: caller.credential.kind,
          userId: caller.userId,
          claim: token,
          updatedAt: now,
        }).onConflictDoUpdate({
          target: [serverAccess.organizationId, serverAccess.credentialId, serverAccess.machineId],
          set: { claim: token, updatedAt: now },
          setWhere: sql`${serverAccess.encryptedCapability} is null and ${serverAccess.revokedAt} is null and ${unheld}`,
        }).returning({ machineId: serverAccess.machineId });
        if (claimed.length > 0) {
          const setStarted = performance.now();
          const management = yield* answered(client.setManagementClient(label).pipe(Effect.timeout(yield* timeLeft)));
          const setMs = performance.now() - setStarted;
          yield* logDialTiming({ stage: "set_management_client", totalMs: setMs, outcome: management.outcome, sampled, correlation });
          if (management.value === null) {
            // Only the claim itself: a key or a revocation written over it meanwhile is somebody else's.
            yield* drizzle.delete(serverAccess)
              .where(and(holder, holding, isNull(serverAccess.encryptedCapability), isNull(serverAccess.revokedAt)));
            return { capability: null, refused: management.refused, revoked: false };
          }
          landed = true;
          const encryptedCapability = encryption.encrypt(management.value);
          const stored = yield* database.transaction(Effect.gen(function* () {
            const { drizzle: tx } = yield* Database;
            yield* lockCredential(caller.credential.id, "shared");
            const kept = yield* tx.update(serverAccess).set({ encryptedCapability, claim: null, updatedAt: now })
              .where(and(holder, holding, isNull(serverAccess.revokedAt)))
              .returning({ machineId: serverAccess.machineId });
            if (kept.length === 0) return null;
            const revoked = (yield* tx.update(serverAccess)
              .set({ revokedAt: now, encryptedCapability: null, claim: null, updatedAt: now })
              .where(and(holder, not(stillAuthorized)))
              .returning({ machineId: serverAccess.machineId })).length > 0;
            return { capability: management.value, refused: null, revoked };
          }));
          if (stored !== null) return stored;
          reclaiming = true;
          continue;
        }
        const [row] = yield* drizzle.select({ encryptedCapability: serverAccess.encryptedCapability, revokedAt: serverAccess.revokedAt })
          .from(serverAccess).where(holder);
        if (row?.encryptedCapability != null) {
          return { capability: yield* decryptPairingSecret(row.encryptedCapability), refused: null, revoked: false };
        }
        if (row?.revokedAt != null) {
          const leased = yield* drizzle.update(serverAccess).set({ claim: token, updatedAt: now })
            .where(and(holder, isNotNull(serverAccess.revokedAt), unheld)).returning({ machineId: serverAccess.machineId });
          if (leased.length > 0) {
            const cleared = yield* answered(client.clearManagementClient(label).pipe(Effect.timeout(yield* timeLeft)));
            if (cleared.value === null) {
              yield* drizzle.update(serverAccess).set({ claim: null }).where(and(holder, holding));
              return { capability: null, refused: cleared.refused, revoked: false };
            }
            yield* drizzle.delete(serverAccess).where(and(holder, holding, isNotNull(serverAccess.revokedAt)));
            yield* drizzle.update(serverAccess).set({ claim: null }).where(and(holder, holding));
            reclaiming = true;
            continue;
          }
        }
        if ((yield* Clock.currentTimeMillis) >= deadline) return { capability: null, refused: null, revoked: false };
        yield* Effect.sleep(CLAIM_POLL);
      }
    }).pipe(Effect.uninterruptible)).pipe(withClusterBudget(SESSION_CLOSE_CLUSTER_MS));
    if (provisioned.revoked) {
      yield* retireServerAccess(caller.credential.id);
      return yield* new Unauthorized();
    }
    return { machine_id: machineId, management: provisioned.capability, refused: provisioned.refused };
  }), { concurrency: "unbounded" });
  const [allocation] = yield* drizzle.select({ assignments: enrollmentAllocation.assignments }).from(enrollmentAllocation)
    .where(and(eq(enrollmentAllocation.organizationId, caller.organization.id), eq(enrollmentAllocation.clusterKey, loaded.generation)));
  return { ok: true as const, value: {
    connections: results.flatMap(({ machine_id, management }) => management === null ? [] : [{ machine_id, management }]),
    unreachable: results.flatMap(({ machine_id, management, refused }) => management === null && refused === null ? [machine_id] : []),
    refused: results.flatMap(({ machine_id, refused }) => refused === null ? [] : [{ machine_id, message: refused }]),
    // A Server the CLI can't reach shows by name where Cloud holds one: the last its Cluster reported, else the one
    // enrollment gave. One Cloud never learned a name for is left out.
    names: Object.fromEntries([
      ...(allocation?.assignments ?? []).filter(isNamed).map(({ machine }) => [machine.id, machine.name]),
      ...loaded.connections.flatMap(({ machine_id, name }) => name === undefined ? [] : [[machine_id, name]]),
    ]),
  } };
});

/** Checked at statement time, not transaction start, so a check after a Set sees an expiry during it. */
const authorizedBy = (userId: SQLWrapper | string, organizationId: SQLWrapper | string, credentialKind: SQLWrapper | string, credentialId: SQLWrapper | string) =>
  sql`(exists (select 1 from ${member}
    where ${member.userId} = ${userId} and ${member.organizationId} = ${organizationId}
      and ${member.role} in ('owner', 'admin'))
  and ((${credentialKind}::text = 'session' and exists (select 1 from ${session}
      where ${session.id} = ${credentialId} and ${session.expiresAt} > ${now}))
    or (${credentialKind}::text = 'token' and exists (select 1 from ${organizationToken}
      where ${organizationToken.id} = ${credentialId}
        and ${organizationToken.organizationId} = ${organizationId}
        and ${organizationToken.expiresAt} > ${now}))))`;
const stillAuthorized = authorizedBy(serverAccess.userId, serverAccess.organizationId, serverAccess.credentialKind, serverAccess.credentialId);

/**
 * Revoke every holder in `scope` whose credential no longer authorizes it (logout, deletion, expiry, membership
 * removal): Cloud forgets its capability at once, then tries one bounded Clear per Server. A Clear that doesn't
 * confirm leaves the revocation pending for the next retry.
 */
export const retireServerAccess = Effect.fn("ServerAccess.retire")(function* (
  /** One credential's holders, on every Server of every Organization it reached; else every holder. */
  credentialId?: string,
) {
  const scope = credentialId === undefined ? undefined : eq(serverAccess.credentialId, credentialId);
  const database = yield* Database;
  const { drizzle } = database;
  yield* database.transaction(Effect.gen(function* () {
    // One credential's revocation waits for its provisioning in flight; a sweep's catches one at the next.
    if (credentialId !== undefined) yield* lockCredential(credentialId, "exclusive");
    const { drizzle: tx } = yield* Database;
    yield* tx.update(serverAccess)
      .set({ revokedAt: now, encryptedCapability: null, claim: null, updatedAt: now })
      .where(and(isNull(serverAccess.revokedAt), not(stillAuthorized), scope));
  }));
  const pending = yield* drizzle.select({
    organizationId: serverAccess.organizationId,
    machineId: serverAccess.machineId,
    credentialId: serverAccess.credentialId,
    cloud: organizationMachine.encryptedCapability,
  }).from(serverAccess)
    .innerJoin(organizationMachine, and(
      eq(organizationMachine.organizationId, serverAccess.organizationId),
      eq(organizationMachine.machineId, serverAccess.machineId),
    ))
    .where(and(isNotNull(serverAccess.revokedAt), scope));
  const outcomes = yield* Effect.forEach(pending, (row) => Effect.gen(function* () {
    const holder = and(
      eq(serverAccess.organizationId, row.organizationId),
      eq(serverAccess.credentialId, row.credentialId),
      eq(serverAccess.machineId, row.machineId),
    );
    const token = crypto.randomUUID();
    // A Clear already in flight on this slot, a sweep's or a first use's, confirms or retries on its own.
    const leased = yield* drizzle.update(serverAccess).set({ claim: token, updatedAt: now })
      .where(and(holder, isNotNull(serverAccess.revokedAt), unheld)).returning({ machineId: serverAccess.machineId });
    if (leased.length === 0) return { machineId: row.machineId, confirmed: false };
    const cleared = yield* Effect.scoped(Effect.gen(function* () {
      const management = yield* decryptPairingSecret(row.cloud);
      const ployz = yield* Ployz;
      const client = yield* ployz.connect({ connections: [{ machine_id: row.machineId, management }], timeoutMs: SERVER_TIMEOUT_MS }, { closeWithin: clusterRead });
      // Clearing an absent or already-cleared slot succeeds, so a retry after a lost reply confirms.
      yield* client.clearManagementClient(serverAccessLabel(row.credentialId));
    })).pipe(Effect.timeout(SERVER_TIMEOUT_MS * 2), withClusterBudget(SESSION_CLOSE_CLUSTER_MS), Effect.as(true), Effect.orElseSucceed(() => false));
    if (cleared) yield* drizzle.delete(serverAccess).where(and(holder, eq(serverAccess.claim, token), isNotNull(serverAccess.revokedAt)));
    yield* drizzle.update(serverAccess).set({ claim: null }).where(and(holder, eq(serverAccess.claim, token)));
    return { machineId: row.machineId, confirmed: cleared };
  }), { concurrency: SERVER_CONCURRENCY });
  return {
    confirmed: outcomes.flatMap((outcome) => outcome.confirmed ? [outcome.machineId] : []),
    unconfirmed: outcomes.flatMap((outcome) => outcome.confirmed ? [] : [outcome.machineId]),
  };
});

/** Revoked credentials of the Organization whose Servers haven't confirmed the Clear yet. */
export const pendingServerRevocations = Effect.fn("ServerAccess.pending")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({
    id: serverAccess.credentialId,
    kind: serverAccess.credentialKind,
    machineId: serverAccess.machineId,
  }).from(serverAccess)
    .where(and(eq(serverAccess.organizationId, organizationId), isNotNull(serverAccess.revokedAt)))
    .orderBy(serverAccess.credentialId, serverAccess.machineId);
  const byCredential = new Map<string, { id: string; kind: "device" | "token"; unconfirmed: MachineId[] }>();
  for (const row of rows) {
    const entry = byCredential.get(row.id)
      ?? { id: row.id, kind: row.kind === "session" ? "device" as const : "token" as const, unconfirmed: [] };
    entry.unconfirmed.push(row.machineId);
    byCredential.set(row.id, entry);
  }
  return [...byCredential.values()];
});
