import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import type { EncryptedSecretValue } from "#/db/tables";
import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { EnrollmentSnapshot, MachineId, RegisterRequest } from "@ployz/sdk";
import { and, eq, isNull } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import {
  enrollmentAllocation,
  organizationMachine,
  machineEnrollmentToken as schemaMachineEnrollmentToken,
} from "#/modules/machines/tables";
import { organizationPairing as schemaOrganizationPairing } from "#/modules/runtime/tables";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { Ployz, PloyzProviderError, ployzVersion, rpcErrorCode } from "#/modules/runtime/ployz.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import {
  enrollmentExpiry,
  MANAGEMENT_CAPABILITY_LENGTH,
  mintedEnrollment,
  registerRequestFromEnrollmentIdentity,
  rustMachineIdSchema,
  waitForFounder,
  type EnrollmentIdentity,
  type EnrollmentCallback,
  type MintMachineEnrollmentInput,
  type ReadMachineEnrollmentInput,
  type ResetPendingEnrollmentInput,
} from "#/modules/machines/enrollment";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createClusterDomainSyncRequestedEvent } from "#/modules/inngest/events";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Unauthorized, Validation } from "#/server/public-error";
import { revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { decryptPairingSecret, loadOrganizationConnections } from "#/modules/machines/connections.server";
import { callStore, cloudStore } from "#/modules/config-store/config-store.server";
import { storeTry } from "#/modules/config-store/store-sdk.server";

const TOKEN_PREFIX = "pmet_";

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

const authorizeEnrollmentOrganization = Effect.fn(
  "MachineEnrollment.authorizeOrganization",
)(function* (actor: Actor, organizationSlug: string) {
  const organization = yield* requireInfrastructureOrganization(
    actor,
    organizationSlug,
  );
  return { organization, userId: actor.userId };
});

const issueEnrollmentToken = Effect.fn("MachineEnrollment.issue")(
  function* (actor: Actor, organizationSlug: string) {
    const { drizzle } = yield* Database;
    const authorization = yield* authorizeEnrollmentOrganization(
      actor,
      organizationSlug,
    );
    const { expiresAt, token } = yield* Effect.sync(() => ({
      expiresAt: enrollmentExpiry(new Date()),
      token: randomSecret(TOKEN_PREFIX),
    }));

    const [issued] = yield* drizzle.insert(schemaMachineEnrollmentToken).values({
      organizationId: authorization.organization.id,
      createdByUserId: authorization.userId,
      tokenHash: hashEnrollmentToken(token),
      expiresAt,
    }).returning({ id: schemaMachineEnrollmentToken.id });
    if (!issued) return yield* Effect.die("Enrollment token insert returned no row");
    return { id: issued.id, token, expiresAt };
  },
);

export const mintMachineEnrollment = Effect.fn("MachineEnrollment.mint")(
  function* (actor: Actor, input: MintMachineEnrollmentInput) {
    const config = yield* AppConfig;
    const { token, expiresAt } = yield* issueEnrollmentToken(actor, input.organizationSlug);
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
  function* (actor: Actor, input: MintMachineEnrollmentInput) {
    const { id, token, expiresAt } = yield* issueEnrollmentToken(actor, input.organizationSlug);
    return { id, token, expiresAt: expiresAt.toISOString() };
  },
);

/** Whether a Server has finished enrolling with one enrollment token. */
export const readMachineEnrollment = Effect.fn("MachineEnrollment.read")(
  function* (actor: Actor, input: ReadMachineEnrollmentInput) {
    const { drizzle } = yield* Database;
    const { organization } = yield* authorizeEnrollmentOrganization(actor, input.organizationSlug);
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
  function* (token: string, machineId: MachineId) {
    const { drizzle } = yield* Database;
    yield* drizzle.update(schemaMachineEnrollmentToken)
      .set({ joinedMachineId: machineId, updatedAt: new Date() })
      .where(and(
        eq(schemaMachineEnrollmentToken.tokenHash, hashEnrollmentToken(token)),
        isNull(schemaMachineEnrollmentToken.joinedMachineId),
      ));
  },
);

export const resetPendingOrganizationEnrollment = Effect.fn(
  "MachineEnrollment.resetPending",
)(function* (actor: Actor, input: ResetPendingEnrollmentInput) {
  const authorization = yield* authorizeEnrollmentOrganization(
    actor,
    input.organizationSlug,
  );
  return yield* resetPendingEnrollment(authorization.organization.id);
});

const verifyEnrollmentToken = Effect.fn("MachineEnrollment.verifyToken")(
  function* (token: string) {
    const { drizzle } = yield* Database;
    const loaded = yield* drizzle
      .select({
        organizationId: schemaMachineEnrollmentToken.organizationId,
        expiresAt: schemaMachineEnrollmentToken.expiresAt,
      })
      .from(schemaMachineEnrollmentToken)
      .where(
        eq(
          schemaMachineEnrollmentToken.tokenHash,
          hashEnrollmentToken(token),
        ),
      )
      .limit(1);
    const row = loaded[0];
    if (!row || row.expiresAt.getTime() <= Date.now()) {
      return yield* new Unauthorized();
    }
    return { organizationId: row.organizationId };
  },
);

type PairingRow = Pick<
  typeof schemaOrganizationPairing.$inferSelect,
  "encryptedPairingSecret" | "founderPublicKey" | "founderMachineId" | "founderClaimMachineId" | "removalStartedAt"
>;

const organizationPairingProjection = {
  encryptedPairingSecret: schemaOrganizationPairing.encryptedPairingSecret,
  founderPublicKey: schemaOrganizationPairing.founderPublicKey,
  founderClaimMachineId: schemaOrganizationPairing.founderClaimMachineId,
  founderMachineId: schemaOrganizationPairing.founderMachineId,
  removalStartedAt: schemaOrganizationPairing.removalStartedAt,
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

// SAFETY: the SDK exports this synchronous Rust policy through CommonJS.
const { allocateEnrollment } = createRequire(import.meta.url)("@ployz/sdk") as Pick<
  typeof PloyzSdk, "allocateEnrollment"
>;

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
    const assignment = yield* Effect.try({
      try: () => allocateEnrollment(input.identity, input.snapshot, history.assignments),
      catch: () => new Conflict({
        message: "Enrollment inputs conflict with saved assignments or the observed subnet pool is exhausted.",
      }),
    });
    if (!history.assignments.some((saved) => saved.machine.id === assignment.machine.id)) {
      // ponytail: rewrite the scoped history; normalize rows if enrollment volume makes this costly.
      yield* drizzle.update(enrollmentAllocation).set({
        assignments: [...history.assignments, assignment],
      }).where(scope);
    }
    return assignment;
  }));
});

const claimOrLoadEnrollment = Effect.fn("MachineEnrollment.claimOrLoad")(
  function* (input: { organizationId: string; publicKey: string; machineId: MachineId }) {
    const database = yield* Database;
    const encryption = yield* SecretEncryption;
    const secret = randomSecret("ppair_");
    return yield* database.transaction(
      Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const [claimed] = yield* drizzle
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
          return {
            kind: "initialize" as const,
            resumed: false,
            pairing: { secret },
          };
        }

        const [current] = yield* drizzle
          .select(organizationPairingProjection)
          .from(schemaOrganizationPairing)
          .where(
            eq(schemaOrganizationPairing.organizationId, input.organizationId),
          )
          .limit(1);
        if (!current) {
          return yield* Effect.die("Organization enrollment disappeared");
        }
        if (current.removalStartedAt !== null) {
          return yield* new Conflict({ message: "Cloud access removal is pending. Confirm endpoint revocation before enrolling again." });
        }
        const pairing = {
          secret: yield* decryptPairingSecret(current.encryptedPairingSecret),
        };
        if (current.founderPublicKey === input.publicKey && current.founderClaimMachineId === input.machineId) {
          return { kind: "initialize" as const, resumed: true, pairing };
        }
        if (current.founderMachineId) {
          return { kind: "ready" as const, pairing };
        }
        return { kind: "pending" as const };
      }),
    );
  },
);

export const enrollMachine = Effect.fn("MachineEnrollment.enrollMachine")(
  function* (input: { token: string; identity: EnrollmentIdentity }) {
    const request = registerRequestFromEnrollmentIdentity(input.identity);
    const token = yield* verifyEnrollmentToken(input.token);
    const state = yield* claimOrLoadEnrollment({
      organizationId: token.organizationId,
      publicKey: input.identity.publicKey,
      machineId: input.identity.machineId,
    });

    if (state.kind === "initialize") {
      return {
        kind: "initialize" as const,
        resumed: state.resumed,
        pairing: state.pairing,
        storage: request.storage,
      };
    }
    if (state.kind === "pending") return waitForFounder();

    return yield* Effect.scoped(Effect.gen(function* () {
      const access = yield* loadOrganizationConnections(token.organizationId);
      if (access.kind === "missing" || access.connections.length === 0) return waitForFounder();
      if (access.generation !== hashEnrollmentToken(state.pairing.secret)) {
        return yield* new Conflict({ message: "The enrollment attempt is no longer current." });
      }
      const opened = yield* (yield* OrganizationRuntime).open(token.organizationId);
      if (opened.status !== "connected") return yield* new PloyzProviderError({ operation: "connect for enrollment", cause: opened });
      const session = opened.connected;
      const snapshot = yield* session.observeEnrollment();
      const database = yield* Database;
      const assignment = yield* database.transaction(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const [current] = yield* drizzle.select(organizationPairingProjection).from(schemaOrganizationPairing)
          .where(eq(schemaOrganizationPairing.organizationId, token.organizationId)).for("update");
        if (!current || current.removalStartedAt !== null || !credentialsMatch(yield* decryptPairingSecret(current.encryptedPairingSecret), state.pairing.secret)) {
          return yield* new Conflict({ message: "The enrollment attempt is no longer current." });
        }
        return yield* reserveEnrollmentAssignment({
          organizationId: token.organizationId, pairing: state.pairing.secret,
          identity: request, snapshot,
        });
      }));
      // The assignment commits before dispatch; a lost response is never replayed.
      const registration = yield* session.register(assignment).pipe(Effect.catch((error): Effect.Effect<never, Conflict | PloyzProviderError> => {
        return rpcErrorCode(error) === "conflict"
          ? Effect.fail(new Conflict({ message: "The saved enrollment assignment conflicts with the Entry Machine's current observation." }))
          : Effect.fail(error);
      }));
      return { kind: "join" as const, pairing: state.pairing, storage: request.storage, registration };
    }));
  },
);

const requireEnrollmentMachine = Effect.fn("MachineEnrollment.requireMachine")(function* (
  organizationId: string, pairing: PairingRow, secret: string, machineId: MachineId,
) {
  if (pairing.founderClaimMachineId === machineId) return;
  const { drizzle } = yield* Database;
  const [allocation] = yield* drizzle.select().from(enrollmentAllocation).where(and(
    eq(enrollmentAllocation.organizationId, organizationId),
    eq(enrollmentAllocation.clusterKey, hashEnrollmentToken(secret)),
  ));
  if (!pairing.founderMachineId || !allocation?.assignments.some((assignment) => assignment.machine.id === machineId)) {
    return yield* new Conflict({ message: "The Machine does not own this enrollment attempt." });
  }
});

/** Publish under the current claim; verify rotated credentials before replacing them. */
export const publishMachineEnrollment = Effect.fn("MachineEnrollment.publishCandidate")(
  function* (input: { token: string; machineId: MachineId; pairingCredential: string; capability: string }) {
    if (input.capability.length === 0 || input.capability.length > MANAGEMENT_CAPABILITY_LENGTH) {
      return yield* new Validation({ message: "Invalid Machine connection capability." });
    }
    const token = yield* verifyEnrollmentToken(input.token);
    const database = yield* Database;
    const encryption = yield* SecretEncryption;
    const publish = (expectedIv?: string) => database.transaction(Effect.gen(function* () {
      yield* verifyEnrollmentToken(input.token);
      const { drizzle } = yield* Database;
      const [pairing] = yield* drizzle.select(organizationPairingProjection)
        .from(schemaOrganizationPairing)
        .where(eq(schemaOrganizationPairing.organizationId, token.organizationId)).for("update");
      if (!pairing || pairing.removalStartedAt !== null) {
        return yield* new Conflict({ message: "The Machine does not own this founding attempt." });
      }
      const secret = yield* decryptPairingSecret(pairing.encryptedPairingSecret);
      if (!credentialsMatch(secret, input.pairingCredential)) {
        return yield* new Conflict({ message: "The founding attempt is no longer current." });
      }
      yield* requireEnrollmentMachine(token.organizationId, pairing, secret, input.machineId);
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
        if (expectedIv === undefined) return saved.encryptedCapability.iv;
        yield* drizzle.update(organizationMachine).set({
          encryptedCapability: encryption.encrypt(input.capability), updatedAt: new Date(),
        }).where(scope);
      } else {
        yield* drizzle.insert(organizationMachine).values({
          organizationId: token.organizationId, machineId: input.machineId,
          clusterKey, encryptedCapability: encryption.encrypt(input.capability), isDialEntry: pairing.founderClaimMachineId === input.machineId,
        }).onConflictDoUpdate({ target: [organizationMachine.organizationId, organizationMachine.machineId],
          set: { clusterKey, encryptedCapability: encryption.encrypt(input.capability), isDialEntry: pairing.founderClaimMachineId === input.machineId, updatedAt: new Date() },
        });
      }
      return null;
    }));
    const expectedIv = yield* publish();
    if (expectedIv !== null) {
      // Shared negotiation confirms the intended Machine; no SQL lock spans the network call.
      yield* Effect.scoped(Effect.gen(function* () {
        yield* (yield* Ployz).connect({
          connections: [{ machine_id: input.machineId, management: input.capability }], timeoutMs: 10_000,
        });
        yield* publish(expectedIv);
      }));
    }
    return { machineId: input.machineId };
  },
);

export const completeMachineEnrollment = Effect.fn(
  "MachineEnrollment.completeMachineEnrollment",
)(
  function* (input: EnrollmentCallback & { token: string }) {
    if ("stage" in input) return yield* publishMachineEnrollment(input);
    const parsed = Schema.decodeUnknownOption(rustMachineIdSchema)(input.machineId);
    if (Option.isNone(parsed)) {
      return yield* new Validation({
        message: "MachineId must be a 32-hex UUID.",
      });
    }
    const machineId = parsed.value;
    const token = yield* verifyEnrollmentToken(input.token);
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
    yield* requireEnrollmentMachine(token.organizationId, row, pairing.secret, machineId);
    const { drizzle } = yield* Database;
    const [candidate] = yield* drizzle.select().from(organizationMachine).where(and(
      eq(organizationMachine.organizationId, token.organizationId),
      eq(organizationMachine.machineId, machineId),
      eq(organizationMachine.clusterKey, hashEnrollmentToken(pairing.secret)),
    )).limit(1);
    if (!candidate) {
      return yield* new Conflict({ message: "Publish the Machine connection before completion." });
    }
    // Shared negotiation verifies machine_id before this transaction can make it ready.
    yield* Effect.scoped(Effect.gen(function* () {
      const opened = yield* (yield* OrganizationRuntime).open(token.organizationId, machineId);
      if (opened.status !== "connected") return yield* new PloyzProviderError({ operation: "confirm enrollment", cause: opened });
    }));

    const database = yield* Database;
    if (row.founderClaimMachineId !== machineId) {
      yield* database.transaction(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const [current] = yield* drizzle.select(organizationPairingProjection).from(schemaOrganizationPairing)
          .where(eq(schemaOrganizationPairing.organizationId, token.organizationId)).for("update");
        if (!current || current.removalStartedAt !== null || !credentialsMatch(yield* decryptPairingSecret(current.encryptedPairingSecret), pairing.secret)) {
          return yield* new Conflict({ message: "The enrollment attempt is no longer current." });
        }
        yield* requireEnrollmentMachine(token.organizationId, current, pairing.secret, machineId);
      }));
      yield* recordJoined(input.token, machineId);
      return { machineId };
    }
    const founded = yield* database.transaction(commitFounder(token.organizationId, machineId, row.encryptedPairingSecret));
    // A Cluster Domain that survived teardown points at the founder once the sync reads the runtime frame;
    // completion never sees the founder's IP, and the hourly sync covers a lost event.
    yield* sendInngestEvent(createClusterDomainSyncRequestedEvent({ organizationId: token.organizationId })).pipe(
      Effect.catch((error) => Effect.logWarning("Cluster Domain sync request failed; enrollment continues.", error)),
    );
    yield* recordJoined(input.token, machineId);
    // ponytail: best effort and once; a completion retried after this commit, or a crash here, deploys nothing (Deploy does).
    if (founded) {
      yield* deployPublished(token.organizationId).pipe(
        Effect.catch((error) => Effect.logWarning("Deploying published Environments to the first Server failed; enrollment continues.", error)),
      );
    }
    return { machineId };
  },
);

/**
 * The Organization's first Server joined: deploy every Environment with published (Saved) state to it, as its Deploy
 * button would. A refusal (a Deploy that would delete data asks first) leaves that Environment for the user.
 */
const deployPublished = Effect.fn("MachineEnrollment.deployPublished")(function* (organizationId: string) {
  const store = yield* cloudStore;
  const { projects } = yield* storeTry(() => store.read(organizationId, { query: "projects" })).pipe(
    Effect.flatMap((view) => view.view === "projects" ? Effect.succeed(view) : Effect.die(new Error("A projects query answered another view"))),
  );
  const environments = projects.flatMap((project) => project.environments.map((environment) => ({ project: project.name, environment })));
  yield* Effect.forEach(environments, (environment) => Effect.gen(function* () {
    const diff = yield* storeTry(() => store.read(organizationId, { query: "diff", environment }));
    if (diff.view !== "diff" || diff.saved === null) return;
    // No member admits it and nothing uploads, so no user is named.
    const result = yield* callStore(organizationId, "", { operation: "write", command: {
      command: "admit", id: crypto.randomUUID(), environment, services: [], version: null, remove: false, accept_volume_loss: [],
    } });
    if (!result.ok) yield* Effect.logInfo(`Not deploying ${environment.project}/${environment.environment} to the first Server: ${result.refusal.message}`);
  }), { discard: true });
});

/**
 * Makes `machineId` the Organization's founder, once, while the founding attempt it confirms is still current; true
 * when this call made it the founder.
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
  if (pairing.founderMachineId !== null) return false;
  yield* drizzle.update(schemaOrganizationPairing).set({ founderMachineId: machineId, updatedAt: new Date() })
    .where(eq(schemaOrganizationPairing.organizationId, organizationId));
  return true;
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
