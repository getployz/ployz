import {
  createCollection,
  localOnlyCollectionOptions,
  type Collection,
  type VirtualRowProps,
} from "@tanstack/react-db";
import { Schema } from "effect";
import { strictParseOptions } from "#/modules/environment-design/schema";
import { parseLiveQueryRow, withoutVirtualProps } from "#/lib/tanstack-db";

/** The connection state of Cloud's one entry-local Runtime Watch. */
export const runtimeLensStatusSchema = Schema.Literals([
  "no_connection",
  "connecting",
  "observed",
  "unavailable",
  "unreachable",
]);

export type RuntimeLensStatus = typeof runtimeLensStatusSchema.Type;

const NonnegativeInt = Schema.Int.check(Schema.isGreaterThanOrEqualTo(0));

/** A directly observed container identity and display detail. It deliberately
 * omits runtime-health interpretation and historical resolved specs. */
export const runtimeContainerRecordSchema = Schema.Struct({
  id: Schema.String,
  displayName: Schema.String,
  machineId: Schema.String,
  projectName: Schema.String,
  kind: Schema.String,
});

export type RuntimeContainerRecord = typeof runtimeContainerRecordSchema.Type;

/** A Machine as the watched entry Machine reported it. `membership` is an open
 * Runtime value; Cloud displays it as evidence and does not turn it into a
 * response or health verdict. */
export const runtimeMachineRecordSchema = Schema.Struct({
  id: Schema.String,
  name: Schema.String,
  publicIp: Schema.NullOr(Schema.String),
  endpoints: Schema.Array(Schema.String),
  membership: Schema.String,
  observedContainerCount: NonnegativeInt,
  observedAt: Schema.String,
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

/** A direct Runtime Watch grouping. `identity` and `serviceId` retain their
 * distinct Engine meanings; neither is a Cloud revision or serving claim. */
export const runtimeServiceRecordSchema = Schema.Struct({
  id: Schema.String,
  identity: Schema.String,
  serviceId: Schema.String,
  containers: Schema.Array(runtimeContainerRecordSchema),
  hookContainers: Schema.Array(runtimeContainerRecordSchema),
  observedAt: Schema.String,
});

export type RuntimeServiceRecord = typeof runtimeServiceRecordSchema.Type;

export function projectRuntimeServiceRecord(
  row: VirtualRowProps | RuntimeServiceRecord,
) {
  // SAFETY: parseLiveQueryRow only drops TanStack's four virtual keys when present.
  return parseLiveQueryRow(
    runtimeServiceRecordSchema,
    row as VirtualRowProps,
  );
}

/** A Machine-local volume identity reported as incomplete evidence. */
const runtimeIncompleteVolumeIdSchema = Schema.Struct({
  machineId: Schema.String,
  name: Schema.String,
});

/** Certificate evidence stays in the Engine's own vocabulary. In particular,
 * a certificate observation does not establish a Route or serving binding. */
export const runtimeCertificateRecordSchema = Schema.Struct({
  hostname: Schema.String,
  status: Schema.String,
  lastError: Schema.NullOr(Schema.String),
  backoff: Schema.NullOr(
    Schema.Struct({
      failureKind: Schema.String,
      nextAttemptAt: Schema.String,
      failures: NonnegativeInt,
    }),
  ),
});

export type RuntimeCertificateRecord =
  typeof runtimeCertificateRecordSchema.Type;

/** Incomplete IDs are evidence of an incomplete observation, never deletion. */
export const runtimeIncompleteIdsSchema = Schema.Struct({
  machines: Schema.Array(Schema.String),
  containers: Schema.Array(Schema.String),
  volumes: Schema.Array(runtimeIncompleteVolumeIdSchema),
  certificates: Schema.Array(Schema.String),
});

export type RuntimeIncompleteIds = typeof runtimeIncompleteIdsSchema.Type;

export const EMPTY_RUNTIME_INCOMPLETE_IDS: RuntimeIncompleteIds = {
  machines: [],
  containers: [],
  volumes: [],
  certificates: [],
};

export const runtimeStatusRecordSchema = Schema.Struct({
  id: Schema.Literal("runtime"),
  status: runtimeLensStatusSchema,
  error: Schema.NullOr(Schema.String),
  /** Hosted DNS hostname as observed by Runtime. Its presence says nothing
   * about DNS publication. */
  hostedDnsHostname: Schema.NullOr(Schema.String),
  certificates: Schema.Array(runtimeCertificateRecordSchema),
  incompleteIds: runtimeIncompleteIdsSchema,
  /** Null when Cloud has not received a Runtime Watch observation. */
  observedAt: Schema.NullOr(Schema.String),
});

export type RuntimeStatusRecord = typeof runtimeStatusRecordSchema.Type;

/** The cached portion of one Runtime Watch observation plus Cloud's connection
 * state. It has no Cloud-generated deployment, DNS, gateway, or health state. */
export const runtimeSnapshotSchema = Schema.Struct({
  status: runtimeLensStatusSchema,
  error: Schema.NullOr(Schema.String),
  hostedDnsHostname: Schema.NullOr(Schema.String),
  machines: Schema.Array(runtimeMachineRecordSchema),
  services: Schema.Array(runtimeServiceRecordSchema),
  certificates: Schema.Array(runtimeCertificateRecordSchema),
  incompleteIds: runtimeIncompleteIdsSchema,
  observedAt: Schema.NullOr(Schema.String),
});

export type RuntimeSnapshot = typeof runtimeSnapshotSchema.Type;

function emptyRuntimeSnapshot(input: {
  status: Exclude<RuntimeLensStatus, "observed" | "unavailable">;
  error: string | null;
}): RuntimeSnapshot {
  return {
    status: input.status,
    error: input.error,
    hostedDnsHostname: null,
    machines: [],
    services: [],
    certificates: [],
    incompleteIds: { ...EMPTY_RUNTIME_INCOMPLETE_IDS },
    observedAt: null,
  };
}

function connectingSnapshot(): RuntimeSnapshot {
  return emptyRuntimeSnapshot({ status: "connecting", error: null });
}

export function noConnectionRuntimeSnapshot(): RuntimeSnapshot {
  return emptyRuntimeSnapshot({ status: "no_connection", error: null });
}

/**
 * Keeps the last observation visible when the EventSource loses its connection.
 * The preserved timestamp makes the stale evidence explicit to consumers.
 */
export function unavailableRuntimeSnapshot(
  previous: RuntimeSnapshot | null,
  error: string,
): RuntimeSnapshot {
  if (previous) {
    return { ...previous, status: "unavailable", error };
  }
  return {
    ...emptyRuntimeSnapshot({ status: "connecting", error }),
    status: "unavailable",
  };
}

export const CLUSTER_UNREACHABLE_ERROR =
  "The cluster is expected but unreachable.";

/** Pairing is present but Cloud cannot dial an entry Machine. Clearing rows
 * prevents prior observations from being rendered as current membership. */
export function unreachableRuntimeSnapshot(error: string): RuntimeSnapshot {
  return emptyRuntimeSnapshot({ status: "unreachable", error });
}

export function applyRuntimeSnapshot(input: {
  organizationSlug: string;
  snapshot: RuntimeSnapshot;
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
      hostedDnsHostname: input.snapshot.hostedDnsHostname,
      certificates: input.snapshot.certificates,
      incompleteIds: input.snapshot.incompleteIds,
      observedAt: input.snapshot.observedAt,
    },
  ]);
}

export function getCachedRuntimeSnapshot(input: {
  organizationSlug: string;
}) {
  const collections = getRuntimeCollections(input);
  const status = collections.status.get("runtime");
  if (!status) return null;
  return Schema.decodeUnknownSync(runtimeSnapshotSchema)({
    status: status.status,
    error: status.error,
    hostedDnsHostname: status.hostedDnsHostname,
    // SAFETY: local-only collection values carry TanStack's four virtual keys at runtime.
    machines: Array.from(collections.machines.values()).map((row) =>
      withoutVirtualProps(row as VirtualRowProps & RuntimeMachineRecord),
    ),
    // SAFETY: local-only collection values carry TanStack's four virtual keys at runtime.
    services: Array.from(collections.services.values()).map((row) =>
      withoutVirtualProps(row as VirtualRowProps & RuntimeServiceRecord),
    ),
    certificates: status.certificates,
    incompleteIds: status.incompleteIds,
    observedAt: status.observedAt,
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
        schema: Schema.toStandardSchemaV1(runtimeMachineRecordSchema, {
          parseOptions: strictParseOptions,
        }),
        initialData: [...snapshot.machines],
      }),
    ),
    status: createCollection(
      localOnlyCollectionOptions({
        id: `runtime:${organizationSlug}:status`,
        getKey: (item: RuntimeStatusRecord) => item.id,
        schema: Schema.toStandardSchemaV1(runtimeStatusRecordSchema, {
          parseOptions: strictParseOptions,
        }),
        initialData: [
          {
            id: "runtime",
            status: snapshot.status,
            error: snapshot.error,
            hostedDnsHostname: snapshot.hostedDnsHostname,
            certificates: snapshot.certificates,
            incompleteIds: snapshot.incompleteIds,
            observedAt: snapshot.observedAt,
          },
        ],
      }),
    ),
    services: createCollection(
      localOnlyCollectionOptions({
        id: `runtime:${organizationSlug}:services`,
        getKey: (item: RuntimeServiceRecord) => item.id,
        schema: Schema.toStandardSchemaV1(runtimeServiceRecordSchema, {
          parseOptions: strictParseOptions,
        }),
        initialData: [...snapshot.services],
      }),
    ),
  };
}

export type RuntimeCollections = ReturnType<
  typeof createRuntimeCollections
>;

export function getRuntimeCollections(input: { organizationSlug: string }) {
  const existing = runtimeCollections.get(input.organizationSlug);
  if (existing) return existing;

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

function replaceRuntimeRows<T extends { id: string }, TKey extends string>(
  collection: Collection<T, TKey>,
  rows: readonly T[],
) {
  const next = new Map(rows.map((row) => [row.id, row]));
  const existingIds = new Set<string>();
  for (const current of collection.values()) {
    existingIds.add(current.id);
    if (!next.has(current.id)) {
      // SAFETY: collections are keyed by row.id, so string ids are TKey.
      collection.delete(current.id as TKey);
    }
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
