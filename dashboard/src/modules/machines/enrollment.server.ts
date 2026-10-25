import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import type { EncryptedSecretValue } from "#/db/tables";
import { allocateEnrollment, type EnrollmentSnapshot, type MachineId, type MachineName, type RegisterRequest } from "@ployz/sdk";
import { and, eq, gt, isNull, lt, ne, or, type SQL, sql } from "drizzle-orm";
import { Cause, Effect, Option, Schema } from "effect";
import {
  enrollmentAllocation,
  organizationMachine,
  machineEnrollmentToken as schemaMachineEnrollmentToken,
} from "#/modules/machines/tables";
import { organizationPairing as schemaOrganizationPairing } from "#/modules/runtime/tables";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess, requireAccessById } from "#/modules/organization/access.server";
import { Ployz, type PloyzProviderError, ployzVersion, RpcError } from "#/modules/runtime/ployz.server";
import { ClusterOutOfTime, clusterRead, OrganizationRuntime, SESSION_CLOSE_CLUSTER_MS, withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import { PostHog } from "#/modules/analytics/posthog.server";
import {
  enrollmentExpiry,
  MANAGEMENT_CAPABILITY_LENGTH,
  mintedEnrollment,
  registerRequestFromEnrollmentIdentity,
  rustMachineIdSchema,
  setupReportAccepted,
  setupReportProperties,
  type SetupReport,
  waitForFounder,
  waitForJoinSlot,
  type EnrollmentIdentity,
  type EnrollmentCallback,
  type EnrollmentCallbackSealing,
  type CliMintMachineEnrollmentInput,
  type CliResetPendingEnrollmentInput,
  type MintMachineEnrollmentInput,
  type ReadMachineEnrollmentInput,
  type ResetPendingEnrollmentInput,
} from "#/modules/machines/enrollment";
import { readOrganizationSealingKey, recordFirstRecoveryBackup, recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { supportsSealingRecovery } from "#/modules/machines/sealing-recovery-compat.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { sendInngestEvent } from "#/modules/inngest/client";
import {
  createClusterDomainSyncRequestedEvent,
  createSealingKeyEnsureRequestedEvent,
  createConfigFirstServerJoinedEvent,
  createObservePushTargetRequestedEvent,
  createServerEnrollmentCompletedEvent,
} from "#/modules/inngest/events";
import type { EnrollmentRole } from "#/modules/machines/waiting/server-enrollment";
import { serverEnrollment, waitingServer } from "#/modules/machines/waiting/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Unauthorized, Unavailable, Validation } from "#/server/public-error";
import { lockOrganizationEnrollment, revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { managementIdentity as managementIdentityOf } from "#/modules/machines/management-identity";
import {
  decryptPairingSecret, foundingClaimFresh, loadOrganizationConnections, SERVER_REACH_TIMEOUT_MS,
} from "#/modules/machines/connections.server";

const TOKEN_PREFIX = "pmet_";

export const CLUSTER_UNREACHABLE = "This Organization's Servers can't be reached. If they were deleted, run `ployz server forget` "
  + "or use Forget Servers in the dashboard.";

const unreachable = () => new Conflict({ message: CLUSTER_UNREACHABLE, userFacing: true });

/** A founding stage's answer while Cloud can't reach the Server; the Server retries it until its own wait ends. */
const cantReachServer = (cause: unknown) => new Unavailable({
  message: "Cloud can't reach this Server through the relay or directly.", userFacing: true, cause,
});

/**
 * A connect no Server answered becomes `cantReachServer`: Cloud's timeout ran out, or the SDK says the dial failed, got
 * no status back, or the entry couldn't dial the Server. A Server that refused keeps its error, since retrying won't help.
 */
const unlessAnswered = (error: PloyzProviderError | Cause.TimeoutError) =>
  Cause.isTimeoutError(error) || (error.cause instanceof RpcError && (error.cause.unanswered || error.cause.targetUnreached))
    ? cantReachServer(error)
    : error;

export function hashEnrollmentToken(token: string) {
  return crypto.createHash("sha256").update(token).digest("hex");
}

function randomSecret(prefix: string) {
  return `${prefix}${crypto.randomBytes(32).toString("base64url")}`;
}

function credentialsMatch(expected: string, actual: string) {
  const digest = (value: string) =>
    crypto.createHash("sha256").update(value).digest();
  return crypto.timingSafeEqual(digest(expected), digest(actual));
}

/** The dashboard names an Organization by its URL's slug, the CLI by its ID. */
type OrganizationRef = { readonly organizationSlug: string } | { readonly organizationId: string };

const authorizeEnrollmentOrganization = Effect.fn(
  "MachineEnrollment.authorizeOrganization",
)(function* (actor: Actor, named: OrganizationRef) {
  const { organization } = "organizationId" in named
    ? yield* requireAccessById(actor, named.organizationId, "servers:add")
    : yield* requireAccess(actor, named.organizationSlug, "servers:add");
  return { organization, userId: actor.userId };
});

export const insertEnrollmentToken = Effect.fn("MachineEnrollment.insertToken")(
  function* (input: {
    readonly organizationId: string;
    readonly userId: string;
    readonly attempt?: { readonly role: EnrollmentRole; readonly serverEnrollmentId: string };
  }) {
    const { drizzle } = yield* Database;
    const { expiresAt, token } = yield* Effect.sync(() => ({
      expiresAt: enrollmentExpiry(new Date()),
      token: randomSecret(TOKEN_PREFIX),
    }));
    const [issued] = yield* drizzle.insert(schemaMachineEnrollmentToken).values({
      organizationId: input.organizationId,
      createdByUserId: input.userId,
      tokenHash: hashEnrollmentToken(token),
      expiresAt,
      enrollmentRole: input.attempt?.role ?? null,
      serverEnrollmentId: input.attempt?.serverEnrollmentId ?? null,
    }).returning({ id: schemaMachineEnrollmentToken.id });
    if (!issued) return yield* Effect.die("Enrollment token insert returned no row");
    return { id: issued.id, token, expiresAt };
  },
);

const issueEnrollmentToken = Effect.fn("MachineEnrollment.issue")(
  function* (actor: Actor, named: OrganizationRef, source: "dashboard" | "cli") {
    const authorization = yield* authorizeEnrollmentOrganization(actor, named);
    const issued = yield* insertEnrollmentToken({
      organizationId: authorization.organization.id,
      userId: authorization.userId,
    });
    yield* (yield* PostHog).capture({
      userId: authorization.userId,
      organizationId: authorization.organization.id,
      event: "server_enrollment_started",
      properties: { source },
    });
    return issued;
  },
);

export const mintMachineEnrollment = Effect.fn("MachineEnrollment.mint")(
  function* (actor: Actor, input: MintMachineEnrollmentInput) {
    const config = yield* AppConfig;
    const { token, expiresAt } = yield* issueEnrollmentToken(actor, input, "dashboard");
    return mintedEnrollment({
      origin: config.app.url.origin,
      token,
      version: ployzVersion(),
      expiresAt,
    });
  },
);

/** The CLI builds its own pasted command, pinned to its own release. */
export const mintCliMachineEnrollment = Effect.fn("MachineEnrollment.mintForCli")(
  function* (actor: Actor, input: CliMintMachineEnrollmentInput) {
    const { id, token, expiresAt } = yield* issueEnrollmentToken(actor, input, "cli");
    return { id, token, expiresAt: expiresAt.toISOString() };
  },
);

/** Whether a Server has finished enrolling with one enrollment token. */
export const readMachineEnrollment = Effect.fn("MachineEnrollment.read")(
  function* (actor: Actor, input: ReadMachineEnrollmentInput) {
    const { drizzle } = yield* Database;
    const { organization } = yield* requireAccessById(actor, input.organizationId, "servers:read");
    const [row] = yield* drizzle
      .select({
        joinedMachineId: schemaMachineEnrollmentToken.joinedMachineId,
        expiresAt: schemaMachineEnrollmentToken.expiresAt,
      })
      .from(schemaMachineEnrollmentToken)
      .where(and(
        eq(schemaMachineEnrollmentToken.id, input.id),
        eq(schemaMachineEnrollmentToken.organizationId, organization.id),
      ))
      .limit(1);
    if (!row) return yield* new NotFound({ message: "Enrollment not found." });
    if (row.joinedMachineId !== null) return { status: "joined" as const, machineId: row.joinedMachineId };
    return { status: row.expiresAt.getTime() <= Date.now() ? "expired" as const : "pending" as const };
  },
);

/** The first Server to complete enrollment with a token is the one that joined through it. */
const recordJoined = Effect.fn("MachineEnrollment.recordJoined")(
  function* (token: string, machineId: MachineId, { founder }: { readonly founder: boolean }) {
    const { drizzle } = yield* Database;
    const joined = yield* drizzle.update(schemaMachineEnrollmentToken)
      .set({ joinedMachineId: machineId, updatedAt: new Date() })
      .where(and(
        eq(schemaMachineEnrollmentToken.tokenHash, hashEnrollmentToken(token)),
        isNull(schemaMachineEnrollmentToken.joinedMachineId),
      ))
      .returning({
        userId: schemaMachineEnrollmentToken.createdByUserId,
        organizationId: schemaMachineEnrollmentToken.organizationId,
      });
    // Credited once, to whoever made the token: a retried completion joins nothing new.
    for (const { userId, organizationId } of joined) {
      yield* (yield* PostHog).capture({ userId, organizationId, event: "server_enrolled", properties: { founder } });
    }
  },
);

/**
 * One setup report becomes one event for whoever made the token, as `server_enrolled` is.
 * Enrollment itself is never touched.
 */
export const recordMachineSetupReport = Effect.fn("MachineEnrollment.recordSetupReport")(
  function* (input: { readonly token: string; readonly report: SetupReport }) {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle
      .select({
        userId: schemaMachineEnrollmentToken.createdByUserId,
        organizationId: schemaMachineEnrollmentToken.organizationId,
        joinedMachineId: schemaMachineEnrollmentToken.joinedMachineId,
        expiresAt: schemaMachineEnrollmentToken.expiresAt,
      })
      .from(schemaMachineEnrollmentToken)
      .where(eq(schemaMachineEnrollmentToken.tokenHash, hashEnrollmentToken(input.token)))
      .limit(1);
    if (!setupReportAccepted(row, input.report.outcome, new Date())) {
      return yield* new NotFound({ message: "Enrollment not found." });
    }
    yield* (yield* PostHog).capture({
      userId: row.userId,
      organizationId: row.organizationId,
      event: `server_setup_${input.report.outcome}`,
      properties: setupReportProperties(input.report),
    });
  },
);

export const resetPendingOrganizationEnrollment = Effect.fn(
  "MachineEnrollment.resetPending",
)(function* (actor: Actor, input: ResetPendingEnrollmentInput | CliResetPendingEnrollmentInput) {
  const authorization = yield* authorizeEnrollmentOrganization(actor, input);
  return yield* resetPendingEnrollment(authorization.organization.id);
});

/** A live token, the Machine identity that claimed it, if one has, and the Machine that joined through it. */
const loadEnrollmentToken = Effect.fn("MachineEnrollment.loadToken")(
  function* (token: string) {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle
      .select({
        organizationId: schemaMachineEnrollmentToken.organizationId,
        claimedMachineId: schemaMachineEnrollmentToken.claimedMachineId,
        claimedPublicKey: schemaMachineEnrollmentToken.claimedPublicKey,
        claimedName: schemaMachineEnrollmentToken.claimedName,
        joinedMachineId: schemaMachineEnrollmentToken.joinedMachineId,
        enrollmentRole: schemaMachineEnrollmentToken.enrollmentRole,
        serverEnrollmentId: schemaMachineEnrollmentToken.serverEnrollmentId,
      })
      .from(schemaMachineEnrollmentToken)
      .where(and(
        eq(schemaMachineEnrollmentToken.tokenHash, hashEnrollmentToken(token)),
        gt(schemaMachineEnrollmentToken.expiresAt, sql`clock_timestamp()`),
      ))
      .limit(1);
    if (!row) return yield* new Unauthorized();
    return row;
  },
);

/**
 * Completion belongs to the one Machine that claimed the token, and may repeat after the token is spent:
 * it hands out nothing.
 */
const requireTokenClaimedBy = Effect.fn("MachineEnrollment.requireTokenClaim")(
  function* (token: string, machineId: MachineId) {
    const row = yield* loadEnrollmentToken(token);
    if (row.claimedMachineId === null || row.claimedPublicKey === null) return yield* new Unauthorized();
    if (row.claimedMachineId !== machineId) {
      return yield* new Conflict({ message: "The Machine does not own this enrollment attempt." });
    }
    return {
      organizationId: row.organizationId,
      claimedPublicKey: row.claimedPublicKey,
      claimedName: row.claimedName,
      spent: row.joinedMachineId !== null,
      serverEnrollmentId: row.serverEnrollmentId,
    };
  },
);

/** Publication also belongs to the claiming Machine, but only until enrollment completes. */
const requireUnspentClaim = Effect.fn("MachineEnrollment.requireUnspentClaim")(
  function* (token: string, machineId: MachineId) {
    const claim = yield* requireTokenClaimedBy(token, machineId);
    if (claim.spent) return yield* new Unauthorized();
    return claim;
  },
);

/** Joins one Organization runs at once behind its founder; the rest wait in line. */
export const JOIN_SLOTS = 8;
/** Inside the 15 minutes a CLI that predates the line keeps asking. */
const JOIN_SLOT_IDLE_LIMIT = sql`interval '10 minutes'`;
/** A queued token keeps its place while it asks again within this long; asking stamps `updatedAt`. */
const QUEUED_LIVE = sql`interval '15 seconds'`;
const claimHoldsSlot = sql`${schemaMachineEnrollmentToken.claimedAt} > clock_timestamp() - ${JOIN_SLOT_IDLE_LIMIT}`;

/**
 * Binds the token to one Machine identity before it hands out the Pairing secret. The same identity
 * may claim again to resume until enrollment completes; a failed enrollment keeps the token spent.
 * Claiming takes a join slot, and renews a claim whose slot lapsed.
 */
const claimEnrollmentToken = Effect.fn("MachineEnrollment.claimToken")(
  function* (token: string, identity: { readonly machineId: MachineId; readonly publicKey: string; readonly name: MachineName }) {
    const { drizzle } = yield* Database;
    const [claimed] = yield* drizzle.update(schemaMachineEnrollmentToken)
      .set({
        claimedMachineId: identity.machineId,
        claimedPublicKey: identity.publicKey,
        claimedName: identity.name,
        claimedAt: sql`case when ${claimHoldsSlot} then ${schemaMachineEnrollmentToken.claimedAt} else now() end`,
        updatedAt: new Date(),
      })
      .where(and(
        eq(schemaMachineEnrollmentToken.tokenHash, hashEnrollmentToken(token)),
        gt(schemaMachineEnrollmentToken.expiresAt, sql`clock_timestamp()`),
        isNull(schemaMachineEnrollmentToken.joinedMachineId),
        or(
          isNull(schemaMachineEnrollmentToken.claimedMachineId),
          and(
            eq(schemaMachineEnrollmentToken.claimedMachineId, identity.machineId),
            eq(schemaMachineEnrollmentToken.claimedPublicKey, identity.publicKey),
          ),
        ),
      ))
      .returning({ organizationId: schemaMachineEnrollmentToken.organizationId });
    if (!claimed) return yield* new Unauthorized();
  },
);

type PairingRow = Pick<
  typeof schemaOrganizationPairing.$inferSelect,
  "encryptedPairingSecret" | "founderPublicKey" | "founderMachineId" | "founderClaimMachineId" | "removalStartedAt" | "createdAt"
>;

const organizationPairingProjection = {
  encryptedPairingSecret: schemaOrganizationPairing.encryptedPairingSecret,
  founderPublicKey: schemaOrganizationPairing.founderPublicKey,
  founderClaimMachineId: schemaOrganizationPairing.founderClaimMachineId,
  founderMachineId: schemaOrganizationPairing.founderMachineId,
  removalStartedAt: schemaOrganizationPairing.removalStartedAt,
  createdAt: schemaOrganizationPairing.createdAt,
};

const loadPairingRow = Effect.fn("MachineEnrollment.loadPairing")(
  function* (organizationId: string) {
    const { drizzle } = yield* Database;
    return yield* drizzle
      .select(organizationPairingProjection)
      .from(schemaOrganizationPairing)
      .where(eq(schemaOrganizationPairing.organizationId, organizationId))
      .limit(1);
  },
);

export const reserveEnrollmentAssignment = Effect.fn(
  "MachineEnrollment.reserveAssignment",
)(function* (input: {
  organizationId: string;
  pairing: string;
  identity: RegisterRequest;
  snapshot: EnrollmentSnapshot;
}) {
  const database = yield* Database;
  const clusterKey = crypto.createHash("sha256").update(input.pairing).digest("hex");
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const scope = and(
      eq(enrollmentAllocation.organizationId, input.organizationId),
      eq(enrollmentAllocation.clusterKey, clusterKey),
    );
    yield* drizzle.insert(enrollmentAllocation).values({
      organizationId: input.organizationId, clusterKey, assignments: [],
    }).onConflictDoNothing();
    const [history] = yield* drizzle.select().from(enrollmentAllocation)
      .where(scope).for("update");
    if (!history) return yield* Effect.die("Enrollment allocation history disappeared");
    const id = input.identity.machine_id;
    const saved = history.assignments.find((assignment) => assignment.machine.id === id);
    const joined = input.snapshot.machines.some((machine) => machine.id === id && machine.runtime.daemon_version !== "");
    const conflict = new Conflict(joined
      ? { message: "This host already joined the Cluster, as a Server that doesn't match this add. To add it again, run ployz-uninstall on it and remove the old Server.", userFacing: true }
      : { message: "Enrollment inputs conflict with saved assignments or the observed subnet pool is exhausted." });
    // A rerun under the same WireGuard key whose Machine never ran its daemon in the Cluster may change its inputs:
    // the earlier attempt's saved assignment and registered row give way, keeping its subnet. Register overwrites the row.
    const retry = () => saved !== undefined && !joined && isDeepStrictEqual(saved.request.public_key, input.identity.public_key)
      ? allocateEnrollment(
        { ...input.identity, assigned_subnet: saved.machine.subnet },
        { ...input.snapshot, machines: input.snapshot.machines.filter((machine) => machine.id !== id) },
        history.assignments.filter((other) => other !== saved),
      )
      : undefined;
    const assignment = yield* Effect.try({
      try: () => {
        try {
          return allocateEnrollment(input.identity, input.snapshot, history.assignments);
        } catch (error) {
          const replaced = retry();
          if (replaced === undefined) throw error;
          return replaced;
        }
      },
      catch: () => conflict,
    });
    if (saved === undefined || !isDeepStrictEqual(saved, assignment)) {
      // ponytail: rewrite the scoped history; normalize rows if enrollment volume makes this costly.
      yield* drizzle.update(enrollmentAllocation).set({
        assignments: [...history.assignments.filter((other) => other !== saved), assignment],
      }).where(scope);
    }
    return assignment;
  }));
});

const countWhere = (condition: SQL | undefined) => sql<number>`count(*) filter (where ${condition})`.mapWith(Number);

/**
 * Admits a join while fewer than `slots` other tokens of the Organization hold a claim and have not
 * joined, after every live waiter that queued first. A token whose own claim still holds a slot goes
 * straight on. Under the `organization_pairing` row lock admissions serialize; without it this is the
 * early answer for a line that is clearly full. A token held back takes its place in line.
 */
const queueForJoinSlot = Effect.fn("MachineEnrollment.queueForJoinSlot")(
  function* (input: { token: string; organizationId: string; founderMachineId: MachineId; joinSlots: number }) {
    const { drizzle } = yield* Database;
    const tokens = schemaMachineEnrollmentToken;
    const tokenHash = hashEnrollmentToken(input.token);
    const own = (column: SQL) => sql`(select ${column} from ${tokens} where ${tokens.tokenHash} = ${tokenHash})`;
    const place = sql`coalesce(${own(sql`${tokens.queuedAt}`)}, clock_timestamp())`;
    const [line] = yield* drizzle.select({
      holding: sql<boolean>`coalesce(${own(claimHoldsSlot)}, false)`,
      joining: countWhere(and(claimHoldsSlot, ne(tokens.claimedMachineId, input.founderMachineId))),
      waiting: countWhere(and(
        sql`not coalesce(${claimHoldsSlot}, false)`,
        lt(tokens.queuedAt, place),
        gt(tokens.updatedAt, sql`clock_timestamp() - ${QUEUED_LIVE}`),
      )),
    }).from(tokens).where(and(
      eq(tokens.organizationId, input.organizationId),
      ne(tokens.tokenHash, tokenHash),
      isNull(tokens.joinedMachineId),
      gt(tokens.expiresAt, sql`clock_timestamp()`),
    ));
    if (line?.holding) return null;
    const first = (line?.joining ?? 0) + (line?.waiting ?? 0);
    if (first < input.joinSlots) return null;
    yield* drizzle.update(tokens)
      .set({ queuedAt: sql`coalesce(${tokens.queuedAt}, clock_timestamp())`, updatedAt: sql`clock_timestamp()` })
      .where(eq(tokens.tokenHash, tokenHash));
    return waitForJoinSlot(first - input.joinSlots + 1);
  },
);

/**
 * Where each attempt waiting in an Organization's join line stands, keyed by attempt: the joins that must finish
 * before it goes, counted the way {@link queueForJoinSlot} answers it, and 0 once a slot is free for its next ask.
 */
export const readJoinStatus = Effect.fn("MachineEnrollment.readJoinStatus")(function* (organizationId: string, joinSlots = JOIN_SLOTS) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select({ founderMachineId: schemaOrganizationPairing.founderMachineId })
    .from(schemaOrganizationPairing).where(eq(schemaOrganizationPairing.organizationId, organizationId));
  const line = new Map<string, number>();
  const founder = pairing?.founderMachineId ?? null;
  if (founder === null) return { ahead: line, joining: 0 };
  const tokens = schemaMachineEnrollmentToken;
  const holding = sql`coalesce(${claimHoldsSlot}, false)`;
  const waiter = sql`not ${holding} and ${tokens.queuedAt} is not null and ${tokens.updatedAt} > clock_timestamp() - ${QUEUED_LIVE}`;
  const rows = yield* drizzle.select({
    serverEnrollmentId: tokens.serverEnrollmentId,
    waiter: sql<boolean>`${waiter}`,
    joining: sql<boolean>`${holding} and ${tokens.claimedMachineId} <> ${founder}`,
    before: sql<number>`rank() over (partition by ${waiter} order by ${tokens.queuedAt}) - 1`.mapWith(Number),
  }).from(tokens).where(and(
    eq(tokens.organizationId, organizationId),
    isNull(tokens.joinedMachineId),
    gt(tokens.expiresAt, sql`clock_timestamp()`),
  ));
  const joining = rows.filter((row) => row.joining).length;
  for (const row of rows) {
    if (row.waiter && row.serverEnrollmentId !== null) line.set(row.serverEnrollmentId, Math.max(0, joining + row.before - joinSlots + 1));
  }
  return { ahead: line, joining };
});

const renewJoinSlot = Effect.fn("MachineEnrollment.renewJoinSlot")(function* (token: string) {
  const { drizzle } = yield* Database;
  const renewed = yield* drizzle.update(schemaMachineEnrollmentToken)
    .set({ claimedAt: sql`now()`, updatedAt: new Date() })
    .where(and(eq(schemaMachineEnrollmentToken.tokenHash, hashEnrollmentToken(token)), claimHoldsSlot))
    .returning({ tokenHash: schemaMachineEnrollmentToken.tokenHash });
  return renewed.length > 0;
});

const holdJoinSlot = Effect.fn("MachineEnrollment.holdJoinSlot")(function* (token: string) {
  if (!(yield* renewJoinSlot(token))) return yield* new Conflict({ message: "The join slot was given up. Ask again." });
});

const releaseUnsentJoinSlot = (token: string, machineId: MachineId, organizationId: string, pairing: string) => Effect.gen(function* () {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const scope = eq(schemaMachineEnrollmentToken.tokenHash, hashEnrollmentToken(token));
    yield* drizzle.select({ tokenHash: schemaMachineEnrollmentToken.tokenHash })
      .from(schemaMachineEnrollmentToken).where(scope).for("update");
    // A new statement after the row lock, so it sees an assignment committed by the request that held it.
    const [allocation] = yield* drizzle.select({ assignments: enrollmentAllocation.assignments }).from(enrollmentAllocation)
      .where(and(
        eq(enrollmentAllocation.organizationId, organizationId),
        eq(enrollmentAllocation.clusterKey, hashEnrollmentToken(pairing)),
      ));
    if (allocation?.assignments.some((saved) => saved.machine.id === machineId)) return;
    yield* drizzle.update(schemaMachineEnrollmentToken)
      .set({ claimedAt: sql`'epoch'::timestamptz`, updatedAt: new Date() })
      .where(and(
        scope,
        eq(schemaMachineEnrollmentToken.claimedMachineId, machineId),
        isNull(schemaMachineEnrollmentToken.joinedMachineId),
      ));
  }));
}).pipe(Effect.ignoreCause({ log: true, message: "Join slot not released" }));

/** A Waiting Server asking for its slot is checking in, so its run does not read it as silent. */
const recordQueuedCheckIn = Effect.fn("MachineEnrollment.recordQueuedCheckIn")(function* (serverEnrollmentId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(waitingServer).set({ lastCheckInAt: sql`now()` }).where(eq(
    waitingServer.id,
    drizzle.select({ id: serverEnrollment.waitingServerId }).from(serverEnrollment).where(eq(serverEnrollment.id, serverEnrollmentId)),
  ));
});

/** The early answer for a joiner that would only be queued, read without the Organization's lock. */
const queueBeforeLocking = Effect.fn("MachineEnrollment.queueBeforeLocking")(
  function* (input: { token: string; organizationId: string; machineId: MachineId; joinSlots: number }) {
    const [pairing] = yield* loadPairingRow(input.organizationId);
    if (!pairing?.founderMachineId || pairing.removalStartedAt !== null || pairing.founderClaimMachineId === input.machineId) {
      return null;
    }
    return yield* queueForJoinSlot({ ...input, founderMachineId: pairing.founderMachineId });
  },
);

const foundingRecovery = Effect.fn("MachineEnrollment.foundingRecovery")(function* (
  organizationId: string, recoverySupport: 1 | undefined,
) {
  const organizationKey = yield* readOrganizationSealingKey(organizationId);
  if (!(yield* supportsSealingRecovery(recoverySupport, organizationKey !== undefined))) return undefined;
  // Only a backup gives a new Cluster a key no Server holds; without one, the founder makes the next key.
  return organizationKey?.recoveryBlob == null
    ? { kind: "back_up" as const }
    : { kind: "seed" as const, expectedPublicKey: organizationKey.publicKey, backup: organizationKey.recoveryBlob };
});

const claimOrLoadEnrollment = Effect.fn("MachineEnrollment.claimOrLoad")(
  function* (input: {
    token: string;
    organizationId: string;
    publicKey: string;
    machineId: MachineId;
    name: MachineName;
    role: EnrollmentRole | null;
    joinSlots: number;
    recoverySupport: 1 | undefined;
  }) {
    const database = yield* Database;
    const encryption = yield* SecretEncryption;
    const secret = randomSecret("ppair_");
    const joiner = input.role === "joiner";
    return yield* database.transaction(
      Effect.gen(function* () {
        yield* lockOrganizationEnrollment(input.organizationId);
        const { drizzle } = yield* Database;
        const [claimed] = joiner ? [] : yield* drizzle
          .insert(schemaOrganizationPairing)
          .values({
            organizationId: input.organizationId,
            encryptedPairingSecret: encryption.encrypt(secret),
            founderPublicKey: input.publicKey,
            founderClaimMachineId: input.machineId,
          })
          .onConflictDoNothing()
          .returning({
            organizationId: schemaOrganizationPairing.organizationId,
          });
        if (claimed) {
          yield* claimEnrollmentToken(input.token, input);
          return {
            kind: "initialize" as const,
            resumed: false,
            pairing: { secret },
            recovery: yield* foundingRecovery(input.organizationId, input.recoverySupport),
          };
        }

        const [current] = yield* drizzle
          .select(organizationPairingProjection)
          .from(schemaOrganizationPairing)
          .where(
            eq(schemaOrganizationPairing.organizationId, input.organizationId),
          )
          .for("update");
        if (!current) {
          if (joiner) return { kind: "pending" as const, fresh: true };
          return yield* Effect.die("Organization enrollment disappeared");
        }
        if (current.removalStartedAt !== null) {
          return yield* new Conflict({ message: "Cloud access removal is pending. Confirm endpoint revocation before enrolling again." });
        }
        const pairing = {
          secret: yield* decryptPairingSecret(current.encryptedPairingSecret),
        };
        if (!joiner && current.founderPublicKey === input.publicKey && current.founderClaimMachineId === input.machineId) {
          yield* claimEnrollmentToken(input.token, input);
          return {
            kind: "initialize" as const,
            resumed: true,
            pairing,
            recovery: yield* foundingRecovery(input.organizationId, input.recoverySupport),
          };
        }
        if (current.founderMachineId) {
          const queued = yield* queueForJoinSlot({ ...input, founderMachineId: current.founderMachineId });
          if (queued) return { kind: "queued" as const, material: queued };
          yield* claimEnrollmentToken(input.token, input);
          return { kind: "ready" as const, pairing };
        }
        return { kind: "pending" as const, fresh: joiner || foundingClaimFresh(current) };
      }),
    );
  },
);

/**
 * A session to the Organization's Cluster and its enrollment snapshot. Its Cluster time is bounded well inside the CLI's
 * request timeout, so a Cluster that never answers is reported, not waited on. Only a connection that fails or doesn't
 * come in time reads unreachable; anything else fails as itself.
 */
export const observeClusterEnrollment = Effect.fn("MachineEnrollment.observeCluster")(function* (organizationId: string) {
  const opened = yield* (yield* OrganizationRuntime).open(organizationId);
  if (opened.status === "unreachable") return { kind: "unreachable" as const };
  if (opened.status === "no_connection") return { kind: "no_connection" as const };
  return { kind: "joined" as const, session: opened.connected, snapshot: yield* opened.connected.observeEnrollment().pipe(clusterRead) };
}, Effect.catchIf((error) => error instanceof ClusterOutOfTime, () => Effect.succeed({ kind: "unreachable" as const })),
withClusterBudget(SERVER_REACH_TIMEOUT_MS));

export const enrollMachine = Effect.fn("MachineEnrollment.enrollMachine")(
  function* (input: { token: string; identity: EnrollmentIdentity; joinSlots?: number }) {
    const request = registerRequestFromEnrollmentIdentity(input.identity);
    const token = yield* loadEnrollmentToken(input.token);
    if (token.joinedMachineId !== null || (token.claimedMachineId !== null
      && (token.claimedMachineId !== input.identity.machineId || token.claimedPublicKey !== input.identity.publicKey))) {
      return yield* new Unauthorized();
    }
    const claim = {
      token: input.token,
      organizationId: token.organizationId,
      publicKey: input.identity.publicKey,
      machineId: input.identity.machineId,
      name: input.identity.name,
      role: token.enrollmentRole,
      joinSlots: input.joinSlots ?? JOIN_SLOTS,
      recoverySupport: input.identity.recoverySupport,
    };
    const early = yield* queueBeforeLocking(claim);
    const state = early ? { kind: "queued" as const, material: early } : yield* claimOrLoadEnrollment(claim);

    if (state.kind === "initialize") {
      return {
        kind: "initialize" as const,
        resumed: state.resumed,
        pairing: state.pairing,
        storage: request.storage,
        recovery: state.recovery,
      };
    }
    // Another Server is founding: wait while it may still finish; an old claim may be a founder that was deleted.
    if (state.kind === "pending") return state.fresh ? waitForFounder() : yield* unreachable();
    if (state.kind === "queued") {
      if (token.serverEnrollmentId !== null) {
        yield* recordQueuedCheckIn(token.serverEnrollmentId).pipe(Effect.ignoreCause({ log: true, message: "Queued check-in not recorded" }));
      }
      return state.material;
    }

    const { pairing } = state;
    // Everything up to dispatch: a failure here means the join was never sent.
    const prepareJoin = Effect.gen(function* () {
      const access = yield* loadOrganizationConnections(token.organizationId);
      // A founder whose connection Cloud doesn't hold can't be joined through.
      if (access.kind === "missing" || access.connections.length === 0) return yield* unreachable();
      if (access.generation !== hashEnrollmentToken(pairing.secret)) {
        return yield* new Conflict({ message: "The enrollment attempt is no longer current." });
      }
      const joined = yield* observeClusterEnrollment(token.organizationId);
      // The pairing went while connecting: Cloud access was removed.
      if (joined.kind === "no_connection") return yield* new Conflict({ message: "The enrollment attempt is no longer current." });
      if (joined.kind === "unreachable") return yield* unreachable();
      const { session, snapshot } = joined;
      const database = yield* Database;
      const assignment = yield* database.transaction(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const [current] = yield* drizzle.select(organizationPairingProjection).from(schemaOrganizationPairing)
          .where(eq(schemaOrganizationPairing.organizationId, token.organizationId)).for("update");
        if (!current || current.removalStartedAt !== null || !credentialsMatch(yield* decryptPairingSecret(current.encryptedPairingSecret), pairing.secret)) {
          return yield* new Conflict({ message: "The enrollment attempt is no longer current." });
        }
        // The slot's write takes the token's row lock first, so the claim read after it sees a cancel that expired the token.
        yield* holdJoinSlot(input.token);
        yield* requireUnspentClaim(input.token, input.identity.machineId);
        return yield* reserveEnrollmentAssignment({
          organizationId: token.organizationId, pairing: pairing.secret,
          identity: request, snapshot,
        });
      }));
      return { session, snapshot, assignment };
    });
    return yield* Effect.scoped(Effect.gen(function* () {
      const { session, snapshot, assignment } = yield* prepareJoin.pipe(
        Effect.onError(() => releaseUnsentJoinSlot(input.token, input.identity.machineId, token.organizationId, pairing.secret)),
      );
      // A row with a daemon version was published by the Machine itself: it joined, and a Register would blank it.
      const joined = snapshot.machines.find((machine) => machine.id === assignment.machine.id && machine.runtime.daemon_version !== "");
      // The assignment commits before dispatch; a lost response is never replayed.
      const registration = joined
        ? { assigned_machine: joined, visible_peers: snapshot.machines.filter((machine) => machine !== joined), target_versions: snapshot.target_versions }
        : yield* session.register(assignment);
      return { kind: "join" as const, pairing, storage: request.storage, registration };
    }));
  },
);

/** `machineId` enrolled in this attempt under the WireGuard key that claimed the token. */
const requireEnrollmentMachine = Effect.fn("MachineEnrollment.requireMachine")(function* (
  organizationId: string, pairing: PairingRow, secret: string, machineId: MachineId, claimedPublicKey: string,
) {
  const notOwned = new Conflict({ message: "The Machine does not own this enrollment attempt." });
  if (pairing.founderClaimMachineId === machineId) {
    if (pairing.founderPublicKey !== claimedPublicKey) return yield* notOwned;
    return;
  }
  const { drizzle } = yield* Database;
  const [allocation] = yield* drizzle.select().from(enrollmentAllocation).where(and(
    eq(enrollmentAllocation.organizationId, organizationId),
    eq(enrollmentAllocation.clusterKey, hashEnrollmentToken(secret)),
  ));
  const assignment = allocation?.assignments.find((saved) => saved.machine.id === machineId);
  if (!pairing.founderMachineId || !assignment
    || Buffer.from(assignment.machine.public_key).toString("base64") !== claimedPublicKey) {
    return yield* notOwned;
  }
});

/** Publish under the current claim; verify rotated credentials before replacing them. */
export const publishMachineEnrollment = Effect.fn("MachineEnrollment.publishCandidate")(
  function* (input: { token: string; machineId: MachineId; pairingCredential: string; capability: string }) {
    if (input.capability.length === 0 || input.capability.length > MANAGEMENT_CAPABILITY_LENGTH) {
      return yield* new Validation({ message: "Invalid Machine connection capability." });
    }
    const token = yield* requireUnspentClaim(input.token, input.machineId);
    const database = yield* Database;
    const encryption = yield* SecretEncryption;
    const managementIdentity = managementIdentityOf(input.capability);
    const publish = (expectedIv?: string) => database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [pairing] = yield* drizzle.select(organizationPairingProjection)
        .from(schemaOrganizationPairing)
        .where(eq(schemaOrganizationPairing.organizationId, token.organizationId)).for("update");
      yield* requireUnspentClaim(input.token, input.machineId);
      if (!pairing || pairing.removalStartedAt !== null) {
        return yield* new Conflict({ message: "The Machine does not own this founding attempt." });
      }
      const secret = yield* decryptPairingSecret(pairing.encryptedPairingSecret);
      if (!credentialsMatch(secret, input.pairingCredential)) {
        return yield* new Conflict({ message: "The founding attempt is no longer current." });
      }
      yield* requireEnrollmentMachine(token.organizationId, pairing, secret, input.machineId, token.claimedPublicKey);
      yield* renewJoinSlot(input.token);
      const scope = and(
        eq(organizationMachine.organizationId, token.organizationId),
        eq(organizationMachine.machineId, input.machineId),
      );
      const clusterKey = hashEnrollmentToken(secret);
      const [saved] = yield* drizzle.select().from(organizationMachine).where(scope);
      if (expectedIv !== undefined && (saved?.clusterKey !== clusterKey || saved.encryptedCapability.iv !== expectedIv)) {
        return yield* new Conflict({ message: "The Machine connection changed during credential verification. Retry enrollment." });
      }
      if (saved?.clusterKey === clusterKey) {
        const previous = yield* decryptPairingSecret(saved.encryptedCapability);
        if (credentialsMatch(previous, input.capability)) return null;
        // A Machine keeps its management key across rotations; a new key would hand Cloud's credential to another endpoint.
        if (managementIdentityOf(previous) !== managementIdentity) {
          return yield* new Conflict({ message: "The Machine's management identity changed. Remove the Server and enroll it again.", userFacing: true });
        }
        if (expectedIv === undefined) return saved.encryptedCapability.iv;
        yield* drizzle.update(organizationMachine).set({
          encryptedCapability: encryption.encrypt(input.capability), managementIdentity, pushTarget: null,
          machineName: token.claimedName ?? saved.machineName, updatedAt: new Date(),
        }).where(scope);
      } else {
        yield* drizzle.insert(organizationMachine).values({
          organizationId: token.organizationId, machineId: input.machineId,
          clusterKey, encryptedCapability: encryption.encrypt(input.capability), managementIdentity, pushTarget: null,
          machineName: token.claimedName, isDialEntry: pairing.founderClaimMachineId === input.machineId,
        }).onConflictDoUpdate({ target: [organizationMachine.organizationId, organizationMachine.machineId],
          set: {
            clusterKey, encryptedCapability: encryption.encrypt(input.capability), managementIdentity, pushTarget: null,
            machineName: sql`coalesce(excluded.machine_name, ${organizationMachine.machineName})`,
            isDialEntry: pairing.founderClaimMachineId === input.machineId, updatedAt: new Date(),
          },
        });
      }
      return null;
    }));
    const expectedIv = yield* publish();
    if (expectedIv !== null) {
      // Shared negotiation confirms the intended Machine; no SQL lock spans the network call. Cloud times the connect
      // itself, since the SDK's own deadline fails like any cancellation.
      yield* Effect.scoped(Effect.gen(function* () {
        yield* (yield* Ployz).connect({
          connections: [{ machine_id: input.machineId, management: input.capability }],
        }, { closeWithin: clusterRead }).pipe(Effect.timeout(SERVER_REACH_TIMEOUT_MS), Effect.mapError(unlessAnswered));
        yield* publish(expectedIv);
      })).pipe(withClusterBudget(SESSION_CLOSE_CLUSTER_MS));
    }
    yield* sendInngestEvent(createObservePushTargetRequestedEvent()).pipe(
      Effect.catch((error) => Effect.logWarning("Push target request failed; the 5-minute sweep sets it.", error)),
    );
    return { machineId: input.machineId };
  },
);

export const completeMachineEnrollment = Effect.fn(
  "MachineEnrollment.completeMachineEnrollment",
)(
  function* (input: EnrollmentCallback & { token: string }) {
    if ("stage" in input) return yield* publishMachineEnrollment(input);
    const { recoveryBackup } = input;
    if (recoveryBackup !== undefined && (input.sealing?.kind !== "held" || input.sealing.publicKey !== recoveryBackup.publicKey)) {
      return yield* new Validation({ message: "The recovery backup is not of the Sealing Key this Server holds." });
    }
    const parsed = Schema.decodeUnknownOption(rustMachineIdSchema)(input.machineId);
    if (Option.isNone(parsed)) {
      return yield* new Validation({
        message: "MachineId must be a 32-hex UUID.",
      });
    }
    const machineId = parsed.value;
    const token = yield* requireTokenClaimedBy(input.token, machineId);
    const loaded = yield* loadPairingRow(token.organizationId);
    const row = loaded[0];
    if (!row || row.removalStartedAt !== null) {
      return yield* new Conflict({
        message: "No founding attempt is pending.",
      });
    }
    const pairing = { secret: yield* decryptPairingSecret(row.encryptedPairingSecret) };
    if (!credentialsMatch(pairing.secret, input.pairingCredential)) {
      return yield* new Conflict({
        message: "The founding attempt is no longer current.",
      });
    }
    yield* requireEnrollmentMachine(token.organizationId, row, pairing.secret, machineId, token.claimedPublicKey);
    const { drizzle } = yield* Database;
    const [candidate] = yield* drizzle.select().from(organizationMachine).where(and(
      eq(organizationMachine.organizationId, token.organizationId),
      eq(organizationMachine.machineId, machineId),
      eq(organizationMachine.clusterKey, hashEnrollmentToken(pairing.secret)),
    )).limit(1);
    if (!candidate) {
      return yield* new Conflict({ message: "Publish the Machine connection before completion." });
    }
    // Shared negotiation verifies machine_id before this transaction can make it ready. Bounded so the Server hears why.
    yield* Effect.scoped(Effect.gen(function* () {
      const opened = yield* (yield* OrganizationRuntime).open(token.organizationId, { only: machineId });
      if (opened.status === "unreachable" && opened.error?.cause instanceof RpcError && unlessAnswered(opened.error) === opened.error) {
        // Still retryable, as every failed completion is, but a Server that answered isn't blamed on the relay.
        return yield* new Unavailable({ message: "This Server refused Cloud's connection.", userFacing: true, cause: opened.error });
      }
      if (opened.status !== "connected") return yield* cantReachServer(opened);
    })).pipe(
      Effect.catchIf((error) => error instanceof ClusterOutOfTime, (error) => Effect.fail(cantReachServer(error))),
      withClusterBudget(SERVER_REACH_TIMEOUT_MS),
    );

    const database = yield* Database;
    if (row.founderClaimMachineId !== machineId) {
      yield* database.transaction(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const [current] = yield* drizzle.select(organizationPairingProjection).from(schemaOrganizationPairing)
          .where(eq(schemaOrganizationPairing.organizationId, token.organizationId)).for("update");
        if (!current || current.removalStartedAt !== null || !credentialsMatch(yield* decryptPairingSecret(current.encryptedPairingSecret), pairing.secret)) {
          return yield* new Conflict({ message: "The enrollment attempt is no longer current." });
        }
        yield* requireEnrollmentMachine(token.organizationId, current, pairing.secret, machineId, token.claimedPublicKey);
        yield* recordSealing(token.organizationId, hashEnrollmentToken(pairing.secret), machineId, input.sealing);
      }));
      yield* recordJoined(input.token, machineId, { founder: false });
      yield* reportAttemptCompleted(token.serverEnrollmentId);
      return { machineId };
    }
    // Past this, `machineId` is the founder (a retried completion may have founded it already).
    const recoveryBackupDigest = yield* database.transaction(Effect.gen(function* () {
      yield* commitFounder(token.organizationId, machineId, row.encryptedPairingSecret);
      yield* requireTokenClaimedBy(input.token, machineId);
      yield* recordSealing(token.organizationId, hashEnrollmentToken(pairing.secret), machineId, input.sealing);
      if (recoveryBackup === undefined) return undefined;
      return yield* recordFirstRecoveryBackup(token.organizationId, recoveryBackup.publicKey, recoveryBackup.backup);
    }));
    // A Cluster Domain that survived teardown points at the founder once the sync reads the runtime frame;
    // completion never sees the founder's IP, and the hourly sync covers a lost event.
    yield* sendInngestEvent(createClusterDomainSyncRequestedEvent({ organizationId: token.organizationId })).pipe(
      Effect.catch((error) => Effect.logWarning("Cluster Domain sync request failed; enrollment continues.", error)),
    );
    yield* recordJoined(input.token, machineId, { founder: true });
    // Its published Environments deploy to it, durably: a retried completion sends this again, and Inngest runs it once.
    yield* sendInngestEvent(createConfigFirstServerJoinedEvent({ organizationId: token.organizationId, machineId })).pipe(
      Effect.catch((error) => Effect.logWarning("Deploying to the first Server was not requested; its Deploy button does it.", error)),
    );
    yield* reportAttemptCompleted(token.serverEnrollmentId);
    return { machineId, recoveryBackupDigest };
  },
);

/**
 * Keeps the first Sealing public key a Machine of this pairing generation reports. A later different key is an
 * error to surface, never a replacement.
 */
const recordSealing = Effect.fn("MachineEnrollment.recordSealing")(function* (
  organizationId: string, clusterKey: string, machineId: MachineId, sealing: EnrollmentCallbackSealing | undefined,
) {
  if (sealing === undefined) return;
  const annotations = { organizationId, machineId };
  if (sealing.kind === "needs_access") {
    yield* Effect.logWarning("A Server joined without its Cluster's Sealing Key.").pipe(Effect.annotateLogs(annotations));
    return yield* sendInngestEvent(createSealingKeyEnsureRequestedEvent({ organizationId })).pipe(
      Effect.catch((error) => Effect.logWarning("Sealing Key backfill request failed; enrollment continues.", error)),
    );
  }
  yield* recordSealingPublicKey(organizationId, clusterKey, sealing.publicKey).pipe(Effect.annotateLogs(annotations));
});

const reportAttemptCompleted = Effect.fn("MachineEnrollment.reportAttemptCompleted")(
  function* (serverEnrollmentId: string | null) {
    if (serverEnrollmentId === null) return;
    yield* sendInngestEvent(createServerEnrollmentCompletedEvent(serverEnrollmentId)).pipe(
      Effect.catch((error) => Effect.logWarning("Server Enrollment completion was not sent; its run reads the token.", error)),
    );
  },
);

/**
 * Makes `machineId` the Organization's founder, once, while the founding attempt it confirms is still current.
 */
const commitFounder = Effect.fn("MachineEnrollment.commitFounder")(function* (
  organizationId: string, machineId: MachineId, encryptedPairingSecret: EncryptedSecretValue,
) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select({
    encryptedPairingSecret: schemaOrganizationPairing.encryptedPairingSecret,
    founderMachineId: schemaOrganizationPairing.founderMachineId,
    removalStartedAt: schemaOrganizationPairing.removalStartedAt,
  }).from(schemaOrganizationPairing).where(eq(schemaOrganizationPairing.organizationId, organizationId)).for("update").limit(1);
  if (!pairing || pairing.removalStartedAt !== null || !isDeepStrictEqual(pairing.encryptedPairingSecret, encryptedPairingSecret)) {
    return yield* new Conflict({ message: "The founding attempt is no longer current." });
  }
  if (pairing.founderMachineId !== null && pairing.founderMachineId !== machineId) {
    return yield* new Conflict({ message: "The Organization is already ready on another Machine." });
  }
  if (pairing.founderMachineId !== null) return;
  yield* drizzle.update(schemaOrganizationPairing).set({ founderMachineId: machineId, updatedAt: new Date() })
    .where(eq(schemaOrganizationPairing.organizationId, organizationId));
});

const resetPendingEnrollment = Effect.fn("MachineEnrollment.resetPendingState")(
  function* (organizationId: string) {
    const [pairing] = yield* loadPairingRow(organizationId);
    if (pairing?.founderMachineId !== null && pairing?.founderMachineId !== undefined) {
      return yield* new Conflict({ message: "A completed enrollment cannot be reset as a pending founder." });
    }
    if (!(yield* revokeOrganizationPairing(organizationId)).confirmed) {
      return yield* new Conflict({ message: "Cloud access is disabled. Endpoint revocation is unconfirmed; the founding claim is retained." });
    }
    return { reset: true as const };
  },
);
