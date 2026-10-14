import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import { and, eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import { canonicalJson, projectJsonValue } from "#/lib/json";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import type { OperationTool } from "#/modules/operations/operations";
import { operationRequest } from "#/modules/operations/tables";
import { Database } from "#/server/database.server";
import { uuidFrom } from "#/server/uuid-from";

export const REQUEST_KEY_HEADER = "x-ployz-request-key";
const REQUEST_KEY = /^[A-Za-z0-9_-]{8,64}$/;

/** What one ask names: the tool, the key it carries (null when it carries none), and the input that key stands for. */
export type OperationRequest<B> = { readonly tool: OperationTool; readonly key: string | null; readonly body: B };
export type Admission = { readonly ok: true; readonly id: string } | { readonly ok: false; readonly refusal: StoreRefusal };
type Refused = Extract<Admission, { ok: false }>;
/** Writes the attempt under `attemptId` with its outbox event, or answers the active attempt it joins. */
export type Admit<E, R> = (attemptId: string) => Effect.Effect<Admission, E, R>;

class AdmissionRefused extends Data.TaggedError("AdmissionRefused")<{ readonly refused: Refused }> {}

const refuse = (code: string, message: string, details: StoreRefusal["details"] = null): Refused =>
  ({ ok: false, refusal: { code, message, details } });

/** The request key a header carries, or a refusal when it isn't one. */
export function requestKeyOf(header: string | null): { ok: true; key: string | null } | Refused {
  if (header === null) return { ok: true, key: null };
  return REQUEST_KEY.test(header)
    ? { ok: true, key: header }
    : refuse("invalid_argument", "A request key is 8 to 64 letters, digits, '-' or '_'.");
}

export const bodyDigest = <B>(tool: OperationTool, body: B) =>
  createHash("sha256").update(canonicalJson({ tool, body: projectJsonValue(body) ?? null })).digest("hex");

/** The attempt a key admits; the Organization is part of it so two Organizations' keys never share one. */
export const attemptIdOf = (organizationId: string, tool: OperationTool, key: string) =>
  uuidFrom(`ployz.operation:${organizationId}:${tool}:${key}`);

const replay = (stored: typeof operationRequest.$inferSelect, tool: OperationTool, digest: string): Admission =>
  stored.tool === tool && stored.digest === digest
    ? { ok: true, id: stored.attemptId }
    : refuse("conflict", "This request key already names a different request. Send a new key.", { attempt: stored.attemptId });

const storedRequest = (organizationId: string, key: string) =>
  Effect.flatMap(Database, ({ drizzle }) =>
    drizzle.select().from(operationRequest)
      .where(and(eq(operationRequest.organizationId, organizationId), eq(operationRequest.requestKey, key)))
      .pipe(Effect.map(([row]) => row)));

/** When request `key` was first admitted, so a replay answers the time the first one did. Null for an unseen key. */
export const admittedAt = (organizationId: string, key: string) =>
  Effect.map(storedRequest(organizationId, key), (row) => row?.createdAt ?? null);

/**
 * One requested operation, admitted at most once per request key. A key seen before answers its attempt without
 * planning again; a new one plans outside the transaction (it may ask Servers), then writes the key, the attempt and
 * its outbox event in one commit. A refusal while writing rolls the key back so a corrected retry can use it.
 */
export const admitOperation = <B, E1, R1, E2, R2>(
  organizationId: string,
  request: OperationRequest<B>,
  plan: Effect.Effect<Refused | Admit<E2, R2>, E1, R1>,
) =>
  Effect.gen(function* () {
    const database = yield* Database;
    const { key, tool } = request;
    const digest = bodyDigest(tool, request.body);
    if (key !== null) {
      const stored = yield* storedRequest(organizationId, key);
      if (stored !== undefined) return replay(stored, tool, digest);
    }
    const planned = yield* plan;
    if ("refusal" in planned) return planned;
    const attemptId = key === null ? crypto.randomUUID() : attemptIdOf(organizationId, tool, key);
    return yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      if (key !== null) {
        const [claimed] = yield* drizzle.insert(operationRequest)
          .values({ organizationId, requestKey: key, tool, digest, attemptId })
          .onConflictDoNothing()
          .returning();
        if (claimed === undefined) {
          const raced = yield* storedRequest(organizationId, key);
          if (raced !== undefined) return replay(raced, tool, digest);
        }
      }
      const admitted = yield* planned(attemptId);
      if (!admitted.ok) return yield* new AdmissionRefused({ refused: admitted });
      if (key !== null && admitted.id !== attemptId) {
        yield* drizzle.update(operationRequest).set({ attemptId: admitted.id })
          .where(and(eq(operationRequest.organizationId, organizationId), eq(operationRequest.requestKey, key)));
      }
      return admitted;
    })).pipe(Effect.catchIf(
      (error): error is AdmissionRefused => error instanceof AdmissionRefused,
      ({ refused }) => Effect.succeed<Admission>(refused),
    ));
  });
