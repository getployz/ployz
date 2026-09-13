import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { EnrollmentSnapshot, MachineId, RegisterRequest } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import {
  enrollmentAllocation,
  organizationMachine,
  machineEnrollmentToken as schemaMachineEnrollmentToken,
} from "#/modules/machines/tables";
import { organizationPairing as schemaOrganizationPairing } from "#/modules/runtime/tables";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { PloyzProviderError } from "#/modules/runtime/ployz.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import {
  enrollmentExpiry,
  mintedEnrollment,
  registerRequestFromEnrollmentIdentity,
  rustMachineIdSchema,
  waitForFounder,
  type EnrollmentIdentity,
  type EnrollmentCallback,
  type MintMachineEnrollmentInput,
  type OrganizationEnrollmentStatus,
  type ResetPendingEnrollmentInput,
} from "#/modules/machines/enrollment";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { commitFirstConnectAdmission } from "#/modules/deployments/first-connect.server";
import { dispatchEnvironmentDeployment } from "#/modules/deployments/dispatch.server";
import { Conflict, Unauthorized, Validation } from "#/server/public-error";
import { revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { decryptPairingSecret, loadOrganizationConnections } from "#/modules/machines/connections.server";

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

export const mintMachineEnrollment = Effect.fn("MachineEnrollment.mint")(
  function* (actor: Actor, input: MintMachineEnrollmentInput) {
    const { drizzle } = yield* Database;
    const config = yield* AppConfig;
    const authorization = yield* authorizeEnrollmentOrganization(
      actor,
      input.organizationSlug,
    );
    const { expiresAt, token } = yield* Effect.sync(() => ({
      expiresAt: enrollmentExpiry(new Date()),
      token: randomSecret(TOKEN_PREFIX),
    }));

    yield* drizzle.insert(schemaMachineEnrollmentToken).values({
      organizationId: authorization.organization.id,
      createdByUserId: authorization.userId,
      tokenHash: hashEnrollmentToken(token),
      expiresAt,
    });

    return mintedEnrollment({
      installerUrl: config.ployz.installerUrl.href,
      origin: config.app.url.origin,
      token,
      expiresAt,
    });
  },
);

export const loadOrganizationEnrollmentStatus = Effect.fn(
  "MachineEnrollment.loadStatus",
)(function* (actor: Actor, input: MintMachineEnrollmentInput) {
  const { drizzle } = yield* Database;
  const authorization = yield* authorizeEnrollmentOrganization(
    actor,
    input.organizationSlug,
  );
  const rows = yield* drizzle
    .select({ founderMachineId: schemaOrganizationPairing.founderMachineId })
    .from(schemaOrganizationPairing)
    .where(
      eq(
        schemaOrganizationPairing.organizationId,
        authorization.organization.id,
      ),
    )
    .limit(1);
  const row = rows[0];
  if (row === undefined) return "unclaimed" satisfies OrganizationEnrollmentStatus;
  return row.founderMachineId === null ? "pending" : "ready";
});

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
        const rpc = Schema.decodeUnknownOption(Schema.Struct({ code: Schema.String }))(error.cause);
        return Option.isSome(rpc) && rpc.value.code === "conflict"
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

/** Publish once against the locked current claim, before any network confirmation. */
export const publishMachineEnrollment = Effect.fn("MachineEnrollment.publishCandidate")(
  function* (input: { token: string; machineId: MachineId; pairingCredential: string; tailcat: string }) {
    if (input.tailcat.length === 0 || input.tailcat.length > 16 * 1024) {
      return yield* new Validation({ message: "Invalid Machine connection capability." });
    }
    const token = yield* verifyEnrollmentToken(input.token);
    const database = yield* Database;
    const encryption = yield* SecretEncryption;
    return yield* database.transaction(Effect.gen(function* () {
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
      if (saved?.clusterKey === clusterKey) {
        const previous = yield* decryptPairingSecret(saved.encryptedTailcat);
        if (!credentialsMatch(previous, input.tailcat)) {
          return yield* new Conflict({ message: "The enrollment attempt already has another connection capability." });
        }
      } else {
        yield* drizzle.insert(organizationMachine).values({
          organizationId: token.organizationId, machineId: input.machineId,
          clusterKey, encryptedTailcat: encryption.encrypt(input.tailcat), isDialEntry: pairing.founderClaimMachineId === input.machineId,
        }).onConflictDoUpdate({ target: [organizationMachine.organizationId, organizationMachine.machineId],
          set: { clusterKey, encryptedTailcat: encryption.encrypt(input.tailcat), isDialEntry: pairing.founderClaimMachineId === input.machineId, updatedAt: new Date() },
        });
      }
      return { machineId: input.machineId };
    }));
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
      return { machineId };
    }
    const deployments = yield* database.transaction(
      commitFirstConnectAdmission({
        organizationId: token.organizationId,
        machineId,
        encryptedPairingSecret: row.encryptedPairingSecret,
      }),
    );
    yield* Effect.forEach(
      deployments,
      (deployment) => dispatchEnvironmentDeployment(deployment),
      { discard: true },
    );
    return { machineId };
  },
);

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
