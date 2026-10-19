import "@tanstack/react-start/server-only";
import type { ConfigStore } from "@ployz/sdk";
import { and, asc, eq, min } from "drizzle-orm";
import { Effect, Stream } from "effect";
import { uploadChunk } from "#/modules/config-store/tables";
import { extractUploadedSource } from "#/modules/github/github-source.server";
import { Database, isUniqueViolation } from "#/server/database.server";
import { StoreRefused } from "./store.contract";
import { storeTry } from "#/modules/config-store/store-sdk.server";

/**
 * The most compressed source one Deployment may upload. Cloudflare in front of Cloud, or of a self-host behind a
 * Tunnel, drops request bodies over 100 MB; this stays under it. The CLI refuses at the same size before it sends.
 */
export const UPLOAD_LIMIT = 95 * 1024 * 1024;
/** Each stored chunk holds at most this much of the upload. */
const CHUNK_SIZE = 1024 * 1024;

const refused = (code: string, message: string) => new StoreRefused({ code, message, details: null });

/** The Store's answer to "does Deployment `deploymentId` exist in this Organization?", as a refusal when it does. */
const notAdmittedYet = (store: ConfigStore, organizationId: string, deploymentId: string) =>
  storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId })).pipe(
    Effect.matchEffect({
      onSuccess: () => Effect.fail(refused("conflict", "This Deployment was admitted already; upload its source before admitting it.")),
      onFailure: (error) => error.code === "not_found" ? Effect.void : Effect.fail(error),
    }),
  );

/**
 * Keep `body`, a gzipped tar of the source, as the upload of Deployment `deploymentId`, which must not be admitted
 * yet. It's written once, whole or not at all, and belongs to that Deployment alone. Resolves to the refusal to
 * answer instead, if any.
 */
export const receiveUpload = Effect.fn("ConfigStore.receiveUpload")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string, body: ReadableStream<Uint8Array> | null,
  limit = UPLOAD_LIMIT,
) {
  const database = yield* Database;
  return yield* Effect.gen(function* () {
    if (body === null) return yield* refused("invalid_argument", "Expected the source as the request body.");
    yield* notAdmittedYet(store, organizationId, deploymentId);
    // Best effort: an upload that can't go now goes with a later one, and never refuses this one.
    yield* releaseUploads(store, organizationId).pipe(
      Effect.catchCause((cause) => Effect.logWarning("Keeping uploads Cloud couldn't release.", cause)),
    );
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      let pending = Buffer.alloc(0);
      let total = 0;
      let index = 0;
      const insert = (data: Buffer) => drizzle.insert(uploadChunk).values({ deploymentId, index: index++, organizationId, data });
      // Failing the stream cancels the body.
      yield* Stream.fromReadableStream({
        evaluate: () => body,
        onError: () => refused("invalid_argument", "The upload stopped before it ended."),
      }).pipe(Stream.runForEach((chunk) => Effect.gen(function* () {
        total += chunk.length;
        if (total > limit) {
          return yield* refused("invalid_argument", `The upload is over the ${limit / 1024 / 1024} MiB compressed source limit.`);
        }
        pending = Buffer.concat([pending, chunk]);
        while (pending.length >= CHUNK_SIZE) {
          yield* insert(pending.subarray(0, CHUNK_SIZE));
          pending = pending.subarray(CHUNK_SIZE);
        }
      })));
      if (total === 0) return yield* refused("invalid_argument", "The upload is empty.");
      if (pending.length > 0) yield* insert(pending);
    })).pipe(Effect.catchIf(isUniqueViolation, () => Effect.fail(refused("conflict", "This Deployment has its upload already."))));
    return undefined;
  }).pipe(Effect.catchTag("StoreRefused", (error) => Effect.succeed(error.refusal)));
});

/**
 * Extract Deployment `deploymentId`'s upload, if Cloud still holds it, into a directory that lasts until the scope
 * closes; resolves to its root, or `undefined` without an upload. A malformed upload fails as `GithubSourceError`.
 */
export const extractUpload = Effect.fn("ConfigStore.extractUpload")(function* (organizationId: string, deploymentId: string) {
  const { drizzle } = yield* Database;
  const owned = and(eq(uploadChunk.organizationId, organizationId), eq(uploadChunk.deploymentId, deploymentId));
  const chunks = yield* drizzle.select({ index: uploadChunk.index }).from(uploadChunk).where(owned).orderBy(asc(uploadChunk.index));
  if (chunks.length === 0) return undefined;
  // One chunk in memory at a time, each read in this Effect's context.
  const run = Effect.runPromiseWith(yield* Effect.context<never>());
  const read = (index: number) => run(drizzle.select({ data: uploadChunk.data }).from(uploadChunk)
    .where(and(owned, eq(uploadChunk.index, index))).pipe(Effect.map((rows) => rows[0]?.data ?? Buffer.alloc(0))));
  async function* data() {
    for (const { index } of chunks) yield await read(index);
  }
  return yield* extractUploadedSource(data());
});

/**
 * Every upload Cloud holds for the Organization, with what the Store says of it: its Environment and digest, and
 * whether a Service or a Deployment that may still run builds from it. `held` is missing for a Deployment the Store
 * doesn't have: one not admitted yet, or whose Environment is gone.
 */
const heldUploads = Effect.fn("ConfigStore.heldUploads")(function* (store: ConfigStore, organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ deploymentId: uploadChunk.deploymentId, arrived: min(uploadChunk.createdAt) })
    .from(uploadChunk).where(eq(uploadChunk.organizationId, organizationId)).groupBy(uploadChunk.deploymentId);
  if (rows.length === 0) return [];
  const { uploads } = yield* storeTry(() => store.read(organizationId, {
    query: "uploads", deployments: rows.map((row) => row.deploymentId),
  }));
  const known = new Map(uploads.map((held) => [held.deployment, held]));
  return rows.map(({ deploymentId, arrived }) => ({ deploymentId, arrived: arrived ?? new Date(0), held: known.get(deploymentId) }));
});

/**
 * The Deployment whose upload Deployment `deploymentId` builds from: itself when it sent one, else one of its
 * Environment's that sent the same content. `undefined` without an upload. Cleanup keeps that one while
 * `deploymentId` may still run, since the Store keeps what a Deployment that may still run builds from.
 */
export const uploadOwner = Effect.fn("ConfigStore.uploadOwner")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string,
) {
  const view = yield* storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId }));
  if (view.upload === null) return undefined;
  const held = yield* heldUploads(store, organizationId);
  if (held.some((upload) => upload.deploymentId === deploymentId)) return deploymentId;
  return held.find(({ held }) => held?.environment_id === view.environment_id && held.digest === view.upload?.digest)
    ?.deploymentId ?? deploymentId;
});

/**
 * Delete every upload of the Organization that nothing builds from any more. Each Service without a source keeps the
 * upload it last built from and its latest new one, until a newer upload for it or its removal; a Deployment that may
 * still run keeps its own. One not admitted yet stays a day.
 */
export const releaseUploads = Effect.fn("ConfigStore.releaseUploads")(function* (store: ConfigStore, organizationId: string) {
  const { drizzle } = yield* Database;
  const dayAgo = Date.now() - 24 * 60 * 60 * 1000;
  for (const { deploymentId, arrived, held } of yield* heldUploads(store, organizationId)) {
    if (held === undefined ? arrived.getTime() > dayAgo : held.kept) continue;
    yield* drizzle.delete(uploadChunk).where(and(eq(uploadChunk.organizationId, organizationId), eq(uploadChunk.deploymentId, deploymentId)));
  }
});
