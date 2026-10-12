import "@tanstack/react-start/server-only";
import type { OffsiteBucket } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { offsiteBucket, volumeOffsite } from "./tables";
import { endpointUrl, type NewBucket, OFFSITE_KEEP, OFFSITE_PREFIX, type OffsiteBucketView, regionOf } from "./volume-offsite";

/** Every column a browser may see; the encrypted secret is never among them. */
const viewColumns = {
  id: offsiteBucket.id,
  endpoint: offsiteBucket.endpoint,
  region: offsiteBucket.region,
  bucket: offsiteBucket.bucket,
  prefix: offsiteBucket.prefix,
  accessKey: offsiteBucket.accessKey,
};

const view = (row: Omit<OffsiteBucketView, "secretSet">): OffsiteBucketView => ({ ...row, secretSet: true });

export const projectBuckets = Effect.fn("OffsiteBucket.project")(function* (organizationId: string, projectId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select(viewColumns).from(offsiteBucket)
    .where(and(eq(offsiteBucket.organizationId, organizationId), eq(offsiteBucket.projectId, projectId)))
    .orderBy(offsiteBucket.createdAt);
  return rows.map(view);
});

/** The wire bucket a typed New bucket becomes; Cloud checks it with the writer before it stores anything. */
export const wireBucket = (typed: NewBucket): OffsiteBucket => {
  const endpoint = endpointUrl(typed.endpoint);
  return {
    endpoint,
    region: regionOf(endpoint),
    bucket: typed.bucket.trim(),
    prefix: OFFSITE_PREFIX,
    access_key: typed.accessKey.trim(),
    secret_key: typed.secretKey,
  };
};

/** Stores a checked bucket once per project; typing the same place again replaces its keys. */
export const saveBucket = Effect.fn("OffsiteBucket.save")(function* (organizationId: string, projectId: string, bucket: OffsiteBucket) {
  const { drizzle } = yield* Database;
  const encryptedSecretKey = (yield* SecretEncryption).encrypt(bucket.secret_key);
  const [row] = yield* drizzle.insert(offsiteBucket).values({
    organizationId,
    projectId,
    endpoint: bucket.endpoint,
    region: bucket.region,
    bucket: bucket.bucket,
    prefix: bucket.prefix,
    accessKey: bucket.access_key,
    encryptedSecretKey,
  }).onConflictDoUpdate({
    target: [offsiteBucket.organizationId, offsiteBucket.projectId, offsiteBucket.endpoint, offsiteBucket.bucket, offsiteBucket.prefix],
    set: { accessKey: bucket.access_key, encryptedSecretKey, updatedAt: new Date() },
  }).returning(viewColumns);
  if (row === undefined) return yield* Effect.die(new Error("The bucket upsert returned no row."));
  return view(row);
});

/** The bucket with its secret, for a request to a Machine; decrypted per request and never stored or logged. */
export const sealedBucket = Effect.fn("OffsiteBucket.sealed")(function* (organizationId: string, bucketId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ ...viewColumns, encryptedSecretKey: offsiteBucket.encryptedSecretKey }).from(offsiteBucket)
    .where(and(eq(offsiteBucket.organizationId, organizationId), eq(offsiteBucket.id, bucketId)));
  if (row === undefined) return yield* new NotFound({ message: "No such offsite bucket." });
  return {
    endpoint: row.endpoint,
    region: row.region,
    bucket: row.bucket,
    prefix: row.prefix,
    access_key: row.accessKey,
    secret_key: (yield* SecretEncryption).decrypt(row.encryptedSecretKey),
  } satisfies OffsiteBucket;
});

export type VolumeOffsiteRow = {
  readonly volumeId: string;
  readonly environmentId: string;
  readonly keep: number;
  readonly uploadedAt: Date | null;
  readonly bucket: OffsiteBucketView;
};

const offsiteColumns = {
  volumeId: volumeOffsite.volumeId,
  environmentId: volumeOffsite.environmentId,
  keep: volumeOffsite.keep,
  uploadedAt: volumeOffsite.uploadedAt,
  ...viewColumns,
};

const offsiteRow = ({ volumeId, environmentId, keep, uploadedAt, ...bucket }: Omit<VolumeOffsiteRow, "bucket"> & Omit<OffsiteBucketView, "secretSet">): VolumeOffsiteRow =>
  ({ volumeId, environmentId, keep, uploadedAt, bucket: view(bucket) });

/** The Organization's offsite copies, optionally one Environment's; each with its bucket, never its secret. */
export const offsiteCopies = Effect.fn("OffsiteBucket.copies")(function* (organizationId: string, environmentId?: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select(offsiteColumns).from(volumeOffsite)
    .innerJoin(offsiteBucket, eq(offsiteBucket.id, volumeOffsite.bucketId))
    .where(and(
      eq(volumeOffsite.organizationId, organizationId),
      environmentId === undefined ? undefined : eq(volumeOffsite.environmentId, environmentId),
    ));
  return rows.map(offsiteRow);
});

/** Every Volume with an offsite copy, across Organizations, for the daily pass. */
export const offsiteVolumeIds = Effect.fn("OffsiteBucket.volumeIds")(function* () {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({ organizationId: volumeOffsite.organizationId, volumeId: volumeOffsite.volumeId }).from(volumeOffsite);
});

export const volumeOffsiteRow = Effect.fn("OffsiteBucket.volume")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select(offsiteColumns).from(volumeOffsite)
    .innerJoin(offsiteBucket, eq(offsiteBucket.id, volumeOffsite.bucketId))
    .where(and(eq(volumeOffsite.organizationId, organizationId), eq(volumeOffsite.volumeId, volumeId)));
  return row === undefined ? null : offsiteRow(row);
});

export const saveVolumeOffsite = Effect.fn("OffsiteBucket.saveVolume")(function* (
  organizationId: string, environmentId: string, volumeId: string, bucketId: string,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(volumeOffsite).values({ volumeId, organizationId, environmentId, bucketId, keep: OFFSITE_KEEP })
    .onConflictDoUpdate({ target: volumeOffsite.volumeId, set: { bucketId, updatedAt: new Date() } });
});

export const markUploaded = Effect.fn("OffsiteBucket.markUploaded")(function* (organizationId: string, volumeId: string, at: Date) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumeOffsite).set({ uploadedAt: at, updatedAt: at })
    .where(and(eq(volumeOffsite.organizationId, organizationId), eq(volumeOffsite.volumeId, volumeId)));
});
