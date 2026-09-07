import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import type { MachineId, RegisterRequest } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Context, Data, Effect, Layer, Option, Redacted, Schema } from "effect";
import { machineEnrollmentToken as schemaMachineEnrollmentToken } from "#/modules/machines/tables";
import { organizationPairing as schemaOrganizationPairing } from "#/modules/runtime/tables";
import type { EncryptedSecretValue, JsonValue } from "#/db/tables";
import type { Actor } from "#/modules/identity/actor";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import {
  Ployz,
  PloyzProviderError,
} from "#/modules/runtime/ployz.server";
import {
  dialAccessFromRelayList,
  enrollmentExpiry,
  heldRegisterSchema,
  mintedEnrollment,
  registerRequestFromEnrollmentIdentity,
  rustMachineIdSchema,
  waitForFounder,
  type CloudPairing,
  type EnrollmentIdentity,
  type HeldRelayList,
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

const TOKEN_PREFIX = "pmet_";

export class PairingSecretDecryptFailure extends Data.TaggedError(
  "PairingSecretDecryptFailure",
)<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

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

const missingDialError = new Validation({
  message:
    "PLOYZ_RELAY_DIAL_CREDENTIAL is required to Dial the Cloud Relay.",
});

const loadEnrollmentSettings = Effect.fn("MachineEnrollment.loadSettings")(
  function* () {
    const config = yield* AppConfig;
    return {
      publicRelayUrl: config.ployz.relayUrl.href,
      deploymentDialUrl: (
        config.ployz.relayPrivateUrl ?? config.ployz.relayUrl
      ).href,
      deploymentDialBearer: config.ployz.relayDialCredential?.pipe(
        Redacted.value,
      ),
    };
  },
);

function deploymentDialBearer(bearer: string | undefined) {
  return bearer ? Effect.succeed(bearer) : Effect.fail(missingDialError);
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
  "encryptedPairingSecret" | "founderPublicKey" | "founderMachineId"
>;

const organizationPairingProjection = {
  encryptedPairingSecret: schemaOrganizationPairing.encryptedPairingSecret,
  founderPublicKey: schemaOrganizationPairing.founderPublicKey,
  founderMachineId: schemaOrganizationPairing.founderMachineId,
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

const decryptPairingSecret = Effect.fn("MachineEnrollment.decryptPairingSecret")(
  function* (value: EncryptedSecretValue) {
    const encryption = yield* SecretEncryption;
    return yield* Effect.try({
      try: () => encryption.decrypt(value),
      catch: (cause) => new PairingSecretDecryptFailure({ cause }),
    });
  },
);

const pairingFromRow = Effect.fn("MachineEnrollment.decodePairing")(
  function* (row: PairingRow) {
    const settings = yield* loadEnrollmentSettings();
    const secret = yield* decryptPairingSecret(row.encryptedPairingSecret);
    return {
      relayUrl: settings.publicRelayUrl,
      secret,
    };
  },
);

const loadPresentCloudPairing = Effect.fn("MachineEnrollment.loadCloudPairing")(
  function* (organizationId: string) {
    const loaded = yield* loadPairingRow(organizationId);
    const row = loaded[0];
    if (!row) return null;
    return yield* pairingFromRow(row);
  },
);

export function heldMachineIds(listed: readonly unknown[]): MachineId[] {
  return listed.flatMap((entry) => {
    const parsed = Schema.decodeUnknownOption(heldRegisterSchema)(entry);
    return Option.isSome(parsed) ? [parsed.value.machineId] : [];
  });
}

export const registerThroughHeldList = Effect.fn(
  "MachineEnrollment.registerThroughHeldList",
)(function* (input: {
  relayUrl: string;
  bearer: string;
  pairing: string;
  held: readonly MachineId[];
  identity: RegisterRequest;
}) {
  const ployz = yield* Ployz;
  const attempts = input.held.map((machineId) =>
    ployz
      .registerHeldMachine(
        input.relayUrl,
        input.bearer,
        input.pairing,
        machineId,
        input.identity,
      )
      .pipe(
        Effect.map((registration) => ({
          kind: "registered" as const,
          registration,
        })),
      ),
  );
  if (attempts.length === 0) {
    return { kind: "not_yet" as const };
  }
  return yield* Effect.firstSuccessOf(attempts).pipe(
    Effect.catch(() => Effect.succeed({ kind: "not_yet" as const })),
  );
});

type EnrollmentRelayInput = {
  relayUrl: string;
  bearer: string;
  pairing: string;
};

type EnrollmentRelayHolding =
  | { readonly kind: "empty" }
  | { readonly kind: "held"; readonly held: readonly MachineId[] };

type EnrollmentRelayRegistrationInput = EnrollmentRelayInput & {
  held: readonly MachineId[];
  identity: RegisterRequest;
};

export interface EnrollmentRelayService {
  readonly inspectHolding: (
    input: EnrollmentRelayInput,
  ) => Effect.Effect<EnrollmentRelayHolding, PloyzProviderError>;
  readonly registerAvailable: (
    input: EnrollmentRelayRegistrationInput,
  ) => Effect.Effect<
    | { readonly kind: "not_yet" }
    | { readonly kind: "registered"; readonly registration: JsonValue }
  >;
  readonly revokeIfEmpty: (
    input: EnrollmentRelayInput,
  ) => Effect.Effect<"revoked" | "held", PloyzProviderError>;
  readonly revokePairing: (
    input: EnrollmentRelayInput,
  ) => Effect.Effect<void, PloyzProviderError>;
}

export class EnrollmentRelay extends Context.Service<
  EnrollmentRelay,
  EnrollmentRelayService
>()("ployz/EnrollmentRelay") {}

const inspectRelayHolding = Effect.fn("MachineEnrollment.inspectRelayHolding")(
  function* (input: EnrollmentRelayInput) {
    const ployz = yield* Ployz;
    const listed = yield* ployz.listHeldRegisters(
      input.relayUrl,
      input.bearer,
      input.pairing,
    );
    if (listed.length === 0) return { kind: "empty" as const };
    const held = heldMachineIds(listed);
    if (held.length === 0) {
      return yield* new PloyzProviderError({
        operation: "list held registers",
        cause: "Cloud could not read any Machine id from the Relay List",
      });
    }
    return { kind: "held" as const, held };
  },
);

export const EnrollmentRelayLive = Layer.effect(
  EnrollmentRelay,
  Effect.gen(function* () {
    const ployz = yield* Ployz;
    const withPloyz = <A, E>(effect: Effect.Effect<A, E, Ployz>) =>
      effect.pipe(Effect.provideService(Ployz, ployz));
    return {
      inspectHolding: (input) => withPloyz(inspectRelayHolding(input)),
      registerAvailable: (input) => withPloyz(registerThroughHeldList(input)),
      revokeIfEmpty: (input) =>
        withPloyz(
          Effect.gen(function* () {
            const holding = yield* inspectRelayHolding(input);
            if (holding.kind === "held") return "held" as const;
            yield* ployz.revokeRelayPairing(
              input.relayUrl,
              input.bearer,
              input.pairing,
            );
            return "revoked" as const;
          }),
        ),
      revokePairing: (input) =>
        ployz.revokeRelayPairing(input.relayUrl, input.bearer, input.pairing),
    } satisfies EnrollmentRelayService;
  }),
);

const observePairingRelayList = Effect.fn("MachineEnrollment.observeRelayList")(
  function* (pairing: CloudPairing) {
    const relay = yield* EnrollmentRelay;
    const settings = yield* loadEnrollmentSettings();
    const bearer = yield* deploymentDialBearer(settings.deploymentDialBearer);
    return yield* relay
      .inspectHolding({
        relayUrl: settings.deploymentDialUrl,
        bearer,
        pairing: pairing.secret,
      })
      .pipe(
        Effect.map((holding) =>
          holding.kind === "empty"
            ? {
                kind: "empty" as const,
                pairing,
                bearer,
              }
            : {
                kind: "held" as const,
                pairing,
                bearer,
                held: holding.held,
              },
        ),
        Effect.catch((error) =>
          Effect.succeed({ kind: "indeterminate" as const, error }),
        ),
      );
  },
);

export const loadOrganizationDialTenant = Effect.fn(
  "MachineEnrollment.loadOrganizationDialTenant",
)(function* (organizationId: string) {
  const pairing = yield* loadPresentCloudPairing(organizationId);
  const list = pairing
    ? yield* observePairingRelayList(pairing)
    : { kind: "missing" as const };
  const settings = yield* loadEnrollmentSettings();
  return dialAccessFromRelayList({
    list,
    dialUrl: settings.deploymentDialUrl,
  });
});

/** Best-effort Relay revoke. An absent pairing is already revoked. */
export const tryRevokeOrganizationRelayPairing = Effect.fn(
  "MachineEnrollment.tryRevokeOrganizationRelayPairing",
)(function* (organizationId: string) {
  const settings = yield* loadEnrollmentSettings();
  return yield* Effect.gen(function* () {
    const pairing = yield* loadPresentCloudPairing(organizationId);
    if (pairing == null) return true;
    const bearer = yield* deploymentDialBearer(settings.deploymentDialBearer);
    const relay = yield* EnrollmentRelay;
    yield* relay.revokePairing({
      relayUrl: settings.deploymentDialUrl,
      bearer,
      pairing: pairing.secret,
    });
    return true;
  }).pipe(Effect.catch(() => Effect.succeed(false)));
});

const claimOrLoadEnrollment = Effect.fn("MachineEnrollment.claimOrLoad")(
  function* (input: { organizationId: string; publicKey: string }) {
    const database = yield* Database;
    const settings = yield* loadEnrollmentSettings();
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
          })
          .onConflictDoNothing()
          .returning({
            organizationId: schemaOrganizationPairing.organizationId,
          });
        if (claimed) {
          return {
            kind: "initialize" as const,
            resumed: false,
            pairing: { relayUrl: settings.publicRelayUrl, secret },
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
        const pairing = {
          relayUrl: settings.publicRelayUrl,
          secret: yield* decryptPairingSecret(current.encryptedPairingSecret),
        };
        if (current.founderMachineId) {
          return { kind: "ready" as const, pairing };
        }
        if (current.founderPublicKey === input.publicKey) {
          return { kind: "initialize" as const, resumed: true, pairing };
        }
        return { kind: "pending" as const };
      }),
    );
  },
);

const joinFromHeldList = Effect.fn("MachineEnrollment.joinFromHeldList")(
  function* (input: {
    request: RegisterRequest;
    listed: HeldRelayList;
  }) {
    const relay = yield* EnrollmentRelay;
    const settings = yield* loadEnrollmentSettings();
    const outcome = yield* relay.registerAvailable({
      relayUrl: settings.deploymentDialUrl,
      bearer: input.listed.bearer,
      pairing: input.listed.pairing.secret,
      held: input.listed.held,
      identity: input.request,
    });
    if (outcome.kind === "not_yet") return waitForFounder();
    return {
      kind: "join" as const,
      pairing: input.listed.pairing,
      storage: input.request.storage,
      registration: outcome.registration,
    };
  },
);

export const enrollMachine = Effect.fn("MachineEnrollment.enrollMachine")(
  function* (input: { token: string; identity: EnrollmentIdentity }) {
    const request = registerRequestFromEnrollmentIdentity(input.identity);
    const token = yield* verifyEnrollmentToken(input.token);
    const state = yield* claimOrLoadEnrollment({
      organizationId: token.organizationId,
      publicKey: input.identity.publicKey,
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

    const list = yield* observePairingRelayList(state.pairing);
    if (list.kind === "indeterminate") return yield* list.error;
    if (list.kind !== "held") return waitForFounder();
    return yield* joinFromHeldList({
      request,
      listed: list,
    });
  },
);

export const completeMachineEnrollment = Effect.fn(
  "MachineEnrollment.completeMachineEnrollment",
)(
  function* (input: {
    token: string;
    machineId: string;
    pairingCredential: string;
  }) {
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
    if (!row) {
      return yield* new Conflict({
        message: "No founding attempt is pending.",
      });
    }
    const pairing = yield* pairingFromRow(row);
    if (!credentialsMatch(pairing.secret, input.pairingCredential)) {
      return yield* new Conflict({
        message: "The founding attempt is no longer current.",
      });
    }
    if (
      row.founderMachineId !== null &&
      row.founderMachineId !== machineId
    ) {
      return yield* new Conflict({
        message: "The Organization is already ready on another Machine.",
      });
    }
    if (row.founderMachineId === null) {
      const list = yield* observePairingRelayList(pairing);
      if (list.kind === "indeterminate") {
        return yield* list.error;
      }
      if (list.kind !== "held" || !list.held.includes(machineId)) {
        return yield* new Conflict({
          message: "The founding Machine is not held on Relay.",
        });
      }
    }

    const database = yield* Database;
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
    const relay = yield* EnrollmentRelay;
    const database = yield* Database;
    const settings = yield* loadEnrollmentSettings();
    return yield* database.transaction(
      Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const [row] = yield* drizzle
          .select(organizationPairingProjection)
          .from(schemaOrganizationPairing)
          .where(eq(schemaOrganizationPairing.organizationId, organizationId))
          .for("update")
          .limit(1);
        if (!row || row.founderMachineId) {
          return yield* new Conflict({
            message: "Only a pending Organization enrollment can be reset.",
          });
        }

        const pairing = yield* pairingFromRow(row);
        const bearer = yield* deploymentDialBearer(settings.deploymentDialBearer);
        const reset = yield* relay.revokeIfEmpty({
          relayUrl: settings.deploymentDialUrl,
          bearer,
          pairing: pairing.secret,
        });
        if (reset === "held") {
          return yield* new Conflict({
            message:
              "The founding Machine is still held on Relay and must resume completion.",
          });
        }

        yield* drizzle
          .delete(schemaOrganizationPairing)
          .where(eq(schemaOrganizationPairing.organizationId, organizationId));
        return { reset: true as const };
      }),
    );
  },
);
