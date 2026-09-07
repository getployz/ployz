import "@tanstack/react-start/server-only";

import { Data, Effect, Schema } from "effect";
import type { EncryptedSecretValue } from "#/db/tables";
import { asRecord } from "#/lib/json";
import {
  type SecretEncryptionService,
} from "#/utils/encrypted-secret.server";
import {
  createPhaseAwareDeployRequest,
  type PhaseAwareDeployRequest,
} from "#/modules/runtime/phase-aware-deploy-contract";
import {
  finiteNumber,
  strictParseOptions,
  trimmedString,
} from "#/modules/environment-design/schema";

export class FrozenDeployInputError extends Data.TaggedError(
  "FrozenDeployInputError",
)<{
  readonly failureCode: "frozen_input_invalid";
  readonly message: string;
  readonly cause?: unknown;
}> {}

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const TrimmedNonEmptyString = trimmedString({ minLength: 1 });
const NonnegativeSafeInteger = finiteNumber({
  integer: true,
  minimum: 0,
  maximum: Number.MAX_SAFE_INTEGER,
});
const PositiveSafeInteger = finiteNumber({
  integer: true,
  minimum: 1,
  maximum: Number.MAX_SAFE_INTEGER,
});
const PositiveU16 = finiteNumber({ integer: true, minimum: 1, maximum: 65_535 });
const ContainerCommand = Schema.Array(Schema.String);
const PositiveUnixSeconds = Schema.String.check(
  Schema.isPattern(/^[1-9][0-9]*$/),
);
const PushedImageReceipt = Schema.Struct({
  index_digest: TrimmedNonEmptyString,
  platforms: Schema.Array(
    Schema.Tuple([
      Schema.Struct({
        os: TrimmedNonEmptyString,
        architecture: TrimmedNonEmptyString,
      }),
      Schema.Struct({
        seed: TrimmedNonEmptyString,
        manifest_digest: TrimmedNonEmptyString,
        image_id: TrimmedNonEmptyString,
        availability_expires_at: PositiveUnixSeconds,
      }),
    ]),
  ).check(Schema.isMinLength(1)),
});
const FrozenVolumeSpec = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("plain") }),
  Schema.Struct({
    kind: Schema.Literal("provisioned"),
    max_size_bytes: PositiveSafeInteger,
  }),
]);

// Persisted versions are selected explicitly. Incompatible SDK changes add a
// version and migration branch; shared additive fields may reuse prior shapes.
const ImageSource = Schema.optionalKey(
  Schema.Union([
    Schema.Struct({ source: Schema.Literal("registry") }),
    Schema.Struct({
      source: Schema.Literal("pushed_to_seed"),
      index_digest: PushedImageReceipt.fields.index_digest,
      platforms: PushedImageReceipt.fields.platforms,
    }),
  ]),
);
const FrozenService = Schema.Struct({
  service_id: NonEmptyString,
  image: NonEmptyString,
  image_source: ImageSource,
  mode: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("replicated"), replicas: PositiveU16 }),
    Schema.Struct({ kind: Schema.Literal("global") }),
  ]),
  runtime: Schema.Struct({
    command: Schema.NullOr(ContainerCommand),
    entrypoint: Schema.NullOr(
      Schema.Union([
        Schema.Literal("clear"),
        Schema.Struct({ argv: ContainerCommand }),
      ]),
    ),
    environment: Schema.Record(Schema.String, Schema.String),
    stop_grace_period: NonnegativeSafeInteger,
    volume_mounts: Schema.optionalKey(
      Schema.Array(
        Schema.Struct({
          volume_name: NonEmptyString,
          target: Schema.String.check(Schema.isStartsWith("/")),
        }),
      ),
    ),
    healthcheck: Schema.optionalKey(
      Schema.NullOr(
        Schema.Struct({
          test: Schema.Union([
            Schema.Literals(["inherit", "disable"]),
            Schema.Struct({ exec: ContainerCommand }),
            Schema.Struct({ shell: Schema.String }),
          ]),
          interval: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
          timeout: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
          retries: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
          start_period: Schema.optionalKey(
            Schema.NullOr(NonnegativeSafeInteger),
          ),
        }),
      ),
    ),
    restart_policy: Schema.optionalKey(
      Schema.Literals([
        "docker-default",
        "no",
        "always",
        "on-failure",
        "unless-stopped",
      ]),
    ),
    cap_add: Schema.optionalKey(Schema.Array(Schema.String)),
    cap_drop: Schema.optionalKey(Schema.Array(Schema.String)),
    resources: Schema.optionalKey(
      Schema.Struct({
        nano_cpus: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
        memory_bytes: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
        pids: Schema.optionalKey(Schema.NullOr(NonnegativeSafeInteger)),
      }),
    ),
  }),
  pre_start: Schema.optionalKey(
    Schema.NullOr(Schema.Struct({ command: ContainerCommand })),
  ),
  depends_on: Schema.optionalKey(
    Schema.Array(
      Schema.Struct({
        service_id: NonEmptyString,
        condition: Schema.Literals(["started", "healthy"]),
      }),
    ),
  ),
  routes: Schema.optionalKey(
    Schema.Array(
      Schema.Struct({
        target: Schema.Union([
          Schema.Struct({
            kind: Schema.Literal("auto_hostname"),
            label: Schema.String,
          }),
          Schema.Struct({
            kind: Schema.Literal("hostname"),
            hostname: Schema.String,
          }),
        ]),
        endpoint_port: PositiveU16,
      }),
    ),
  ),
});
const FrozenTargetV1 = Schema.Struct({
  namespace_id: NonEmptyString,
  origin: Schema.optionalKey(Schema.NullOr(Schema.String)),
  services: Schema.Array(FrozenService),
});
const FrozenTargetV2 = Schema.Struct({
  namespace_id: FrozenTargetV1.fields.namespace_id,
  origin: FrozenTargetV1.fields.origin,
  services: FrozenTargetV1.fields.services,
  volumes: Schema.Record(NonEmptyString, FrozenVolumeSpec),
});
const RegistryCredentials = Schema.Record(
  Schema.String,
  Schema.Union([
    Schema.Struct({
      kind: Schema.Literal("basic"),
      username: Schema.String,
      password: Schema.String,
    }),
    Schema.Struct({
      kind: Schema.Literal("identity_token"),
      token: Schema.String,
    }),
  ]),
);
const FrozenDeployInputV1 = Schema.Struct({
  version: Schema.Literal(1),
  target: FrozenTargetV1,
  registryCredentials: RegistryCredentials,
  volumeCount: NonnegativeSafeInteger,
});
const FrozenDeployInputV2 = Schema.Struct({
  version: Schema.Literal(2),
  target: FrozenTargetV2,
  registryCredentials: RegistryCredentials,
  volumeCount: NonnegativeSafeInteger,
});
const FrozenDeployInputV3 = Schema.Struct({
  version: Schema.Literal(3),
  request: Schema.Struct({
    version: Schema.Literal(1),
    target: FrozenTargetV2,
    phases: Schema.Array(
      Schema.Struct({
        services: Schema.Array(
          Schema.Struct({
            service_id: NonEmptyString,
            requirement: Schema.Literals(["required", "opportunistic"]),
          }),
        ),
      }),
    ),
  }),
  registryCredentials: RegistryCredentials,
  volumeCount: NonnegativeSafeInteger,
});

type ParsedFrozenDeployInputV1 = typeof FrozenDeployInputV1.Type;
type ParsedFrozenDeployInputV2 = typeof FrozenDeployInputV2.Type;
type ParsedFrozenDeployInputV3 = typeof FrozenDeployInputV3.Type;
type FrozenDeployRequest = typeof FrozenTargetV2.Type;

export type FrozenDeployInput = {
  readonly version: 3;
  readonly request: PhaseAwareDeployRequest<FrozenDeployRequest>;
  readonly registryCredentials: ParsedFrozenDeployInputV3["registryCredentials"];
  readonly volumeCount: number;
};

function legacyPhaseAwareRequest(
  target: FrozenDeployRequest,
): PhaseAwareDeployRequest<FrozenDeployRequest> {
  return {
    version: 1,
    target,
    phases:
      target.services.length === 0
        ? []
        : [
            {
              services: target.services.map((service) => ({
                service_id: service.service_id,
                requirement: "required" as const,
              })),
            },
          ],
  };
}

function restoreFrozenDeployInputV1(
  value: ParsedFrozenDeployInputV1,
): FrozenDeployInput {
  const targetWithoutOrigin = {
    namespace_id: value.target.namespace_id,
    volumes: Object.fromEntries(
      value.target.services.flatMap((service) =>
        (service.runtime.volume_mounts ?? []).map((mount) => [
          mount.volume_name,
          { kind: "plain" as const },
        ]),
      ),
    ),
    services: value.target.services,
  };
  const target: FrozenDeployRequest =
    value.target.origin === undefined
      ? targetWithoutOrigin
      : { ...targetWithoutOrigin, origin: value.target.origin };
  return {
    version: 3,
    request: legacyPhaseAwareRequest(target),
    registryCredentials: value.registryCredentials,
    volumeCount: value.volumeCount,
  };
}

function restoreFrozenDeployTargetV2(
  target: ParsedFrozenDeployInputV2["target"],
): FrozenDeployRequest {
  const restored = {
    namespace_id: target.namespace_id,
    volumes: Object.fromEntries(
      Object.keys(target.volumes).map((name) => [
        name,
        { kind: "plain" as const },
      ]),
    ),
    services: target.services,
  };
  return target.origin === undefined
    ? restored
    : { ...restored, origin: target.origin };
}

function restoreFrozenDeployInputV2(
  value: ParsedFrozenDeployInputV2,
): FrozenDeployInput {
  const target = restoreFrozenDeployTargetV2(value.target);
  return {
    version: 3,
    request: legacyPhaseAwareRequest(target),
    registryCredentials: value.registryCredentials,
    volumeCount: value.volumeCount,
  };
}

function restoreFrozenDeployInputV3(value: ParsedFrozenDeployInputV3) {
  const target = restoreFrozenDeployTargetV2(value.request.target);
  return createPhaseAwareDeployRequest({
    target,
    phases: value.request.phases,
  }).pipe(
    Effect.map(
      (request) =>
        ({
          version: 3,
          request,
          registryCredentials: value.registryCredentials,
          volumeCount: value.volumeCount,
        }) satisfies FrozenDeployInput,
    ),
    Effect.mapError(
      (cause) =>
        new FrozenDeployInputError({
          failureCode: "frozen_input_invalid",
          message: "Frozen deploy input is invalid.",
          cause,
        }),
    ),
  );
}

function decodeVersion<S extends Schema.ConstraintDecoder<unknown>>(
  schema: S,
  value: Schema.Json,
) {
  return Schema.decodeUnknownEffect(schema)(value, strictParseOptions).pipe(
    Effect.mapError(
      (cause) =>
        new FrozenDeployInputError({
          failureCode: "frozen_input_invalid",
          message: "Frozen deploy input is invalid.",
          cause,
        }),
    ),
  );
}

function parseVersionedFrozenDeployInput(
  value: Schema.Json,
): Effect.Effect<FrozenDeployInput, FrozenDeployInputError> {
  const record = asRecord(value);
  if (record === null || !("version" in record)) {
    return Effect.fail(
      new FrozenDeployInputError({
        failureCode: "frozen_input_invalid",
        message: "Frozen deploy input is invalid.",
      }),
    );
  }
  switch (record["version"]) {
    case 1:
      return decodeVersion(FrozenDeployInputV1, value).pipe(
        Effect.map(restoreFrozenDeployInputV1),
      );
    case 2:
      return decodeVersion(FrozenDeployInputV2, value).pipe(
        Effect.map(restoreFrozenDeployInputV2),
      );
    case 3:
      return decodeVersion(FrozenDeployInputV3, value).pipe(
        Effect.flatMap(restoreFrozenDeployInputV3),
      );
    default:
      return Effect.fail(
        new FrozenDeployInputError({
          failureCode: "frozen_input_invalid",
          message: "Frozen deploy input version is unsupported.",
        }),
      );
  }
}

export function decodeFrozenDeployInput(
  encryption: SecretEncryptionService,
  value: EncryptedSecretValue | null | undefined,
): Effect.Effect<FrozenDeployInput | null, FrozenDeployInputError> {
  if (!value) return Effect.succeed(null);
  return Effect.try({
    // SAFETY: JSON.parse can only produce JSON values for valid ciphertext plaintext.
    try: () => JSON.parse(encryption.decrypt(value)) as Schema.Json,
    catch: (cause) =>
      new FrozenDeployInputError({
        failureCode: "frozen_input_invalid",
        message: "Frozen deploy input is invalid.",
        cause,
      }),
  }).pipe(Effect.flatMap(parseVersionedFrozenDeployInput));
}

export function encodeFrozenDeployInput(
  encryption: SecretEncryptionService,
  input: FrozenDeployInput,
): Effect.Effect<EncryptedSecretValue, FrozenDeployInputError> {
  return parseVersionedFrozenDeployInput(input).pipe(
    Effect.flatMap((parsed) =>
      Effect.try({
        try: () => encryption.encrypt(JSON.stringify(parsed)),
        catch: (cause) =>
          new FrozenDeployInputError({
            failureCode: "frozen_input_invalid",
            message: "Frozen deploy input could not be encrypted.",
            cause,
          }),
      }),
    ),
  );
}

export function redactFrozenDeployManifest(input: FrozenDeployInput) {
  const target = input.request.target;
  return {
    version: 2 as const,
    namespaceId: target.namespace_id,
    volumes: Object.keys(target.volumes).map((name) => ({ name })),
    services: target.services.map((service) => ({
      serviceId: service.service_id,
      image: service.image,
      mode: service.mode,
      environmentKeys: Object.keys(service.runtime.environment).sort(),
      hasRegistryCredential: service.service_id in input.registryCredentials,
    })),
  };
}
