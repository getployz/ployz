import {
  createCollection,
  localOnlyCollectionOptions,
  type Collection,
  type VirtualRowProps,
} from "@tanstack/react-db";
import { Schema } from "effect";
import { strictParseOptions } from "#/modules/environment-design/schema";
import { parseLiveQueryRow, withoutVirtualProps } from "#/lib/tanstack-db";

export const runtimeLensStatusSchema = Schema.Literals([
  "no_connection",
  "connecting",
  "live_empty",
  "live_rows",
  "unavailable",
  "unreachable",
]);

export type RuntimeLensStatus = typeof runtimeLensStatusSchema.Type;

const NonnegativeInt = Schema.Int.check(Schema.isGreaterThanOrEqualTo(0));
const NonEmptyString = Schema.String.check(Schema.isNonEmpty());

export const runtimeGatewayTestimonySchema = Schema.Union([
  Schema.Struct({
    status: Schema.Literal("current"),
    routeCount: NonnegativeInt,
  }),
  Schema.Struct({
    status: Schema.Literal("last_known_good"),
    routeCount: NonnegativeInt,
  }),
  Schema.Struct({
    status: Schema.Literal("unavailable"),
    routeCount: NonnegativeInt,
  }),
  Schema.Struct({
    status: Schema.Literal("silent"),
    reason: Schema.Literals(["no_answer", "not_reported"]),
  }),
  Schema.Struct({ status: Schema.Literal("not_installed") }),
]);

export type RuntimeGatewayTestimony = typeof runtimeGatewayTestimonySchema.Type;

export const runtimeMachineRecordSchema = Schema.Struct({
  id: NonEmptyString,
  name: NonEmptyString,
  publicIp: Schema.NullOr(Schema.String),
  gateway: runtimeGatewayTestimonySchema,
  observedContainerCount: Schema.NullOr(NonnegativeInt),
  region: Schema.NullOr(Schema.String),
  availabilityZone: Schema.NullOr(Schema.String),
  overlayIp: Schema.NullOr(Schema.String),
  endpoints: Schema.Array(Schema.String),
  testimonyStatus: Schema.Literals(["answered", "no_answer"]),
  lastObservedAt: Schema.NullOr(Schema.String),
  updatedAt: Schema.String,
});

export type RuntimeMachineRecord = typeof runtimeMachineRecordSchema.Type;

export function projectRuntimeMachineRecord(
  row: VirtualRowProps | RuntimeMachineRecord,
) {
  // SAFETY: parseLiveQueryRow only drops TanStack's four virtual keys when present.
  return parseLiveQueryRow(
    runtimeMachineRecordSchema,
    row as VirtualRowProps,
  );
}

// Automatic-hostname namespace mode (#462): disabled, Ployz-managed, or a custom
// user suffix.
export const runtimePublicUrlModeSchema = Schema.Literals([
  "disabled",
  "ployz",
  "custom",
]);

/** A hostname Rust is serving for a service, its origin (user-declared vs the
 * managed automatic binding), and whether its TLS certificate is available. */
export const runtimeRouteBindingSchema = Schema.Struct({
  id: NonEmptyString,
  hostname: Schema.String,
  origin: Schema.Literals(["declared", "automatic"]),
  tls: Schema.Union([
    Schema.Struct({
      status: Schema.Literal("available"),
      certificateId: NonEmptyString,
    }),
    Schema.Struct({ status: Schema.Literal("unavailable") }),
    Schema.Struct({ status: Schema.Literal("unknown") }),
  ]),
});
export type RuntimeRouteBinding = typeof runtimeRouteBindingSchema.Type;

/** Cluster-level public-URL state. `domain` is the managed lease suffix
 * (e.g. `brisk-river.up.ployz.app`), null until the lease is acquired. */
export const runtimePublicUrlSchema = Schema.Struct({
  mode: runtimePublicUrlModeSchema,
  domain: Schema.NullOr(Schema.String),
  leaseApex: Schema.NullOr(Schema.String),
  dnsTarget: Schema.Struct({
    intent: Schema.Literals(["enabled", "disabled"]),
    allocation: Schema.Literals(["unacquired", "allocated"]),
    publication: Schema.Literals(["unpublished", "applied", "withdrawn"]),
  }),
});
export type RuntimePublicUrl = typeof runtimePublicUrlSchema.Type;

export const runtimeServiceRecordSchema = Schema.Struct({
  id: NonEmptyString,
  namespaceId: NonEmptyString,
  serviceId: NonEmptyString,
  activeRevisionId: NonEmptyString,
  routeCount: NonnegativeInt,
  instanceCount: NonnegativeInt,
  readyInstanceCount: NonnegativeInt,
  // Route bindings Rust is actually serving for this service (origin-tagged, with
  // per-binding TLS availability).
  bindings: Schema.Array(runtimeRouteBindingSchema),
  updatedAt: Schema.String,
});

export type RuntimeServiceRecord = typeof runtimeServiceRecordSchema.Type;

export const runtimeStatusRecordSchema = Schema.Struct({
  id: Schema.Literal("runtime"),
  status: runtimeLensStatusSchema,
  error: Schema.NullOr(Schema.String),
  publicUrl: runtimePublicUrlSchema,
  updatedAt: Schema.String,
});

export type RuntimeStatusRecord = typeof runtimeStatusRecordSchema.Type;

export const runtimeSnapshotLensSchema = Schema.Struct({
  status: runtimeLensStatusSchema,
  error: Schema.NullOr(Schema.String),
  publicUrl: runtimePublicUrlSchema,
  machines: Schema.Array(runtimeMachineRecordSchema),
  services: Schema.Array(runtimeServiceRecordSchema),
  updatedAt: Schema.String,
});

export type RuntimeSnapshotLens = typeof runtimeSnapshotLensSchema.Type;

export const RUNTIME_PUBLIC_URL_NONE: RuntimePublicUrl = {
  mode: "disabled",
  domain: null,
  leaseApex: null,
  dnsTarget: {
    intent: "disabled",
    allocation: "unacquired",
    publication: "unpublished",
  },
};

function connectingSnapshot(): RuntimeSnapshotLens {
  return {
    status: "connecting",
    error: null,
    publicUrl: RUNTIME_PUBLIC_URL_NONE,
    machines: [],
    services: [],
    updatedAt: new Date().toISOString(),
  };
}

/**
 * Builds an `unavailable` lens that keeps the previous machines/services and
 * updatedAt so stale rows stay visible while the runtime is down. Only when
 * there is no previous snapshot does it stamp the current time.
 */
export function unavailableRuntimeSnapshot(
  previous: RuntimeSnapshotLens | null,
  error: string,
): RuntimeSnapshotLens {
  return {
    status: "unavailable",
    error,
    publicUrl: previous?.publicUrl ?? RUNTIME_PUBLIC_URL_NONE,
    machines: previous?.machines ?? [],
    services: previous?.services ?? [],
    updatedAt: previous?.updatedAt ?? new Date().toISOString(),
  };
}

export const CLUSTER_UNREACHABLE_ERROR =
  "The cluster is expected but unreachable.";

/**
 * Pairing is present but Cloud cannot Dial. Clear Machine rows so a stale
 * `organization_machine` leftover is never rendered as membership.
 */
export function unreachableRuntimeSnapshot(
  error: string,
): RuntimeSnapshotLens {
  return {
    status: "unreachable",
    error,
    publicUrl: RUNTIME_PUBLIC_URL_NONE,
    machines: [],
    services: [],
    updatedAt: new Date().toISOString(),
  };
}

export function applyRuntimeSnapshot(input: {
  organizationSlug: string;
  snapshot: RuntimeSnapshotLens;
}) {
  const collections = getRuntimeCollections({
    organizationSlug: input.organizationSlug,
  });
  replaceRuntimeRows(collections.machines, input.snapshot.machines);
  replaceRuntimeRows(collections.services, input.snapshot.services);
  replaceRuntimeRows(collections.status, [
    {
      id: "runtime",
      status: input.snapshot.status,
      error: input.snapshot.error,
      publicUrl: input.snapshot.publicUrl,
      updatedAt: input.snapshot.updatedAt,
    },
  ]);
}

export function getCachedRuntimeSnapshot(input: {
  organizationSlug: string;
}) {
  const collections = getRuntimeCollections(input);
  const status = collections.status.get("runtime");
  if (!status) return null;
  return Schema.decodeUnknownSync(runtimeSnapshotLensSchema)({
    status: status.status,
    error: status.error,
    publicUrl: status.publicUrl,
    // SAFETY: local-only collection values carry TanStack's four virtual keys at runtime.
    machines: Array.from(collections.machines.values()).map((row) =>
      withoutVirtualProps(row as VirtualRowProps & RuntimeMachineRecord),
    ),
    // SAFETY: local-only collection values carry TanStack's four virtual keys at runtime.
    services: Array.from(collections.services.values()).map((row) =>
      withoutVirtualProps(row as VirtualRowProps & RuntimeServiceRecord),
    ),
    updatedAt: status.updatedAt,
  });
}

const runtimeCollections = new Map<string, RuntimeCollections>();

function createRuntimeCollections(organizationSlug: string) {
  const snapshot = connectingSnapshot();
  return {
    machines: createCollection(
      localOnlyCollectionOptions({
        id: `runtime:${organizationSlug}:machines`,
        getKey: (item: RuntimeMachineRecord) => item.id,
        schema: Schema.toStandardSchemaV1(runtimeMachineRecordSchema, { parseOptions: strictParseOptions }),
        initialData: [...snapshot.machines],
      }),
    ),
    status: createCollection(
      localOnlyCollectionOptions({
        id: `runtime:${organizationSlug}:status`,
        getKey: (item: RuntimeStatusRecord) => item.id,
        schema: Schema.toStandardSchemaV1(runtimeStatusRecordSchema, { parseOptions: strictParseOptions }),
        initialData: [
          {
            id: "runtime",
            status: snapshot.status,
            error: snapshot.error,
            publicUrl: snapshot.publicUrl,
            updatedAt: snapshot.updatedAt,
          },
        ],
      }),
    ),
    services: createCollection(
      localOnlyCollectionOptions({
        id: `runtime:${organizationSlug}:services`,
        getKey: (item: RuntimeServiceRecord) => item.id,
        schema: Schema.toStandardSchemaV1(runtimeServiceRecordSchema, { parseOptions: strictParseOptions }),
        initialData: [...snapshot.services],
      }),
    ),
  };
}

export type RuntimeCollections = ReturnType<
  typeof createRuntimeCollections
>;

export function getRuntimeCollections(input: {
  organizationSlug: string;
}) {
  const existing = runtimeCollections.get(input.organizationSlug);
  if (existing) {
    return existing;
  }

  const collections = createRuntimeCollections(input.organizationSlug);
  runtimeCollections.set(input.organizationSlug, collections);
  return collections;
}

export async function preloadRuntimeCollections(input: {
  organizationSlug: string;
}) {
  const collections = getRuntimeCollections(input);
  await Promise.allSettled([
    collections.machines.preload(),
    collections.status.preload(),
    collections.services.preload(),
  ]);
}

function replaceRuntimeRows<
  T extends { id: string },
  TKey extends string,
>(
  collection: Collection<T, TKey>,
  rows: readonly T[],
) {
  const next = new Map(rows.map((row) => [row.id, row]));
  const existingIds = new Set<string>();
  for (const current of collection.values()) {
    existingIds.add(current.id);
    if (!next.has(current.id))
      // SAFETY: collections are keyed by row.id, so string ids are TKey.
      collection.delete(current.id as TKey);
  }
  for (const row of rows) {
    if (existingIds.has(row.id)) {
      // SAFETY: collections are keyed by row.id, so string ids are TKey.
      collection.update(row.id as TKey, (draft) => Object.assign(draft, row));
    } else {
      collection.insert(row);
    }
  }
}
