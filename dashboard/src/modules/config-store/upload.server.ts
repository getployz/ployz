import "@tanstack/react-start/server-only";
import type { ConfigStore } from "@ployz/sdk";
import { and, asc, eq, min } from "drizzle-orm";
import { Effect, Option, Stream } from "effect";
import { uploadChunk } from "#/modules/config-store/tables";
import { extractUploadedSource } from "#/modules/github/github-source.server";
import { Database, isUniqueViolation } from "#/server/database.server";
import { StoreRefused } from "./store.contract";
import { refusedWith, storeTry } from "#/modules/config-store/store-sdk.server";

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

/** What Cloud reads of a Deployment to tell which upload it builds from. */
interface Builds {
  id: string;
  environment_id: string;
  number: number;
  in_flight: boolean;
  upload: { digest: string } | null;
}

/** Whether two Deployments of one Environment build from the same upload: the same content, by digest. */
const sameUpload = (a: Builds, b: Builds) =>
  a.upload !== null && b.upload !== null && a.environment_id === b.environment_id && a.upload.digest === b.upload.digest;

/** Every upload Cloud holds for the Organization, with its Deployment while the Store still has it. */
const heldUploads = Effect.fn("ConfigStore.heldUploads")(function* (store: ConfigStore, organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ deploymentId: uploadChunk.deploymentId, arrived: min(uploadChunk.createdAt) })
    .from(uploadChunk).where(eq(uploadChunk.organizationId, organizationId)).groupBy(uploadChunk.deploymentId);
  return yield* Effect.forEach(rows, ({ deploymentId, arrived }) =>
    storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId })).pipe(
      Effect.map((view) => Option.some(view)),
      Effect.catchIf(refusedWith("not_found"), () => Effect.succeed(Option.none())),
      Effect.map((view) => ({ deploymentId, arrived: arrived ?? new Date(0), view })),
    ));
});

/**
 * The Deployment whose upload Deployment `deploymentId` builds from: the newest one Cloud holds with the same content
 * in its Environment, which may be an earlier Deployment, or itself. `undefined` without an upload.
 */
export const uploadOwner = Effect.fn("ConfigStore.uploadOwner")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string,
) {
  const view = yield* storeTry(() => store.read(organizationId, { query: "deployment", id: deploymentId }));
  if (view.upload === null) return undefined;
  const owners = (yield* heldUploads(store, organizationId)).flatMap(({ view: held }) =>
    Option.isSome(held) && sameUpload(held.value, view) ? [held.value] : []);
  return owners.reduce<Builds | undefined>((newest, held) => newest === undefined || held.number > newest.number ? held : newest, undefined)?.id
    ?? deploymentId;
});

/**
 * Delete every upload of the Organization that can no longer build. An upload stays while its Deployment may still
 * run, or a Deployment of its Environment builds from it that may still run or is the newest; when two hold the same
 * content, the newer one stays. One not admitted yet stays a day. One Store read per upload, and one per Environment.
 */
export const releaseUploads = Effect.fn("ConfigStore.releaseUploads")(function* (store: ConfigStore, organizationId: string) {
  const { drizzle } = yield* Database;
  const held = yield* heldUploads(store, organizationId);
  const environments = new Map<string, readonly Builds[]>();
  const recent = Effect.fn("ConfigStore.uploadEnvironment")(function* (view: Builds & { environment: { project: string; name: string } }) {
    const known = environments.get(view.environment_id);
    if (known !== undefined) return known;
    const { project, name } = view.environment;
    const deployments = yield* storeTry(() => store.read(organizationId, {
      query: "deployments", environment: { project, environment: name }, limit: 20, cursor: null,
    })).pipe(
      Effect.map((page): readonly Builds[] => page.deployments),
      Effect.catchIf(refusedWith("not_found"), () => Effect.succeed([])),
    );
    environments.set(view.environment_id, deployments);
    return deployments;
  });
  for (const { deploymentId, arrived, view } of held) {
    const kept = yield* Effect.gen(function* () {
      if (Option.isNone(view)) return arrived.getTime() > Date.now() - 24 * 60 * 60 * 1000;
      if (view.value.in_flight) return true;
      const newer = held.some(({ view: other }) =>
        Option.isSome(other) && sameUpload(other.value, view.value) && other.value.number > view.value.number);
      if (view.value.upload === null || newer) return false;
      const deployments = yield* recent(view.value);
      return deployments.some((deployment, index) => (index === 0 || deployment.in_flight) && sameUpload(deployment, view.value));
    });
    if (kept) continue;
    yield* drizzle.delete(uploadChunk).where(and(eq(uploadChunk.organizationId, organizationId), eq(uploadChunk.deploymentId, deploymentId)));
  }
});
