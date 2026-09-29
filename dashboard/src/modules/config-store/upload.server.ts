import "@tanstack/react-start/server-only";
import type { ConfigStore } from "@ployz/sdk";
import { and, asc, eq, sql } from "drizzle-orm";
import { Data, Effect, Stream } from "effect";
import { uploadChunk } from "#/modules/config-store/tables";
import { extractUploadedSource } from "#/modules/github/github-source.server";
import { Database, isUniqueViolation } from "#/server/database.server";
import type { StoreRefusal } from "./store.contract";
import { refusedWith, storeTry } from "#/modules/config-store/store-sdk.server";
import { IN_FLIGHT, isInFlight } from "./store-deployments";

/** The most compressed source one Deployment may upload: the same cap as a repository archive's download. */
const UPLOAD_LIMIT = 256 * 1024 * 1024;
/** Each stored chunk holds at most this much of the upload. */
const CHUNK_SIZE = 1024 * 1024;

class UploadRefused extends Data.TaggedError("UploadRefused")<{ readonly refusal: StoreRefusal }> {}

const refused = (code: string, message: string) => new UploadRefused({ refusal: { code, message, details: null } });

/** The Store's answer to "does Deployment `deploymentId` exist in this Organization?", as a refusal when it does. */
const notAdmittedYet = (store: ConfigStore, organizationId: string, deploymentId: string) =>
  storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId })).pipe(
    Effect.matchEffect({
      onSuccess: () => Effect.fail(refused("conflict", "This Deployment was admitted already; upload its source before admitting it.")),
      onFailure: (error) => error.code === "not_found" ? Effect.void : Effect.fail(refused(error.code, error.message)),
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
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      // ponytail: uploads never admitted go a day later, with the next upload of their Organization.
      yield* drizzle.delete(uploadChunk).where(and(
        eq(uploadChunk.organizationId, organizationId),
        sql`${uploadChunk.createdAt} < now() - interval '1 day'`,
        sql`${uploadChunk.deploymentId} not in (select id from config_deployment
          where status in (${sql.join(IN_FLIGHT.map((status) => sql`${status}`), sql`, `)}))`,
      ));
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
  }).pipe(Effect.catchTag("UploadRefused", (error) => Effect.succeed(error.refusal)));
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
  // One chunk in memory at a time.
  const read = (index: number) => Effect.runPromise(drizzle.select({ data: uploadChunk.data }).from(uploadChunk)
    .where(and(owned, eq(uploadChunk.index, index))).pipe(Effect.map((rows) => rows[0]?.data ?? Buffer.alloc(0))));
  async function* data() {
    for (const { index } of chunks) yield await read(index);
  }
  return yield* extractUploadedSource(data());
});

/** Delete Deployment `deploymentId`'s upload once it has ended, or was never admitted; while it's in flight, keep it. */
export const releaseUpload = Effect.fn("ConfigStore.releaseUpload")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string,
) {
  const status = yield* storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId })).pipe(
    Effect.map((view) => view.status),
    Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(undefined)),
  );
  if (status !== undefined && isInFlight(status)) return;
  const { drizzle } = yield* Database;
  yield* drizzle.delete(uploadChunk).where(and(eq(uploadChunk.organizationId, organizationId), eq(uploadChunk.deploymentId, deploymentId)));
});
