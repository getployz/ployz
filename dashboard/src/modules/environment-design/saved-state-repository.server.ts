import "@tanstack/react-start/server-only";

import { and, asc, desc, eq } from "drizzle-orm";
import { Effect, Schema } from "effect";
import {
  environmentSavedStateSnapshot as schemaEnvironmentSavedStateSnapshot,
} from "#/modules/deployments/tables";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { strictParseOptions } from "./schema";
import {
  decodePersistedSavedEnvironmentIntent,
  decodePersistedSavedEnvironmentState,
} from "./saved-intent";
import { destructiveVolumeReviewsSchema } from "./destructive-volume-review";

const savedStateColumns = {
  id: schemaEnvironmentSavedStateSnapshot.id,
  environmentId: schemaEnvironmentSavedStateSnapshot.environmentId,
  actorId: schemaEnvironmentSavedStateSnapshot.actorId,
  intent: schemaEnvironmentSavedStateSnapshot.intent,
  volumeDeletionAuthorizations:
    schemaEnvironmentSavedStateSnapshot.volumeDeletionAuthorizations,
};

const decodeSavedState = Effect.fn("EnvironmentDesign.decodeSavedState")(
  function* (row: {
    id: string;
    environmentId: string;
    actorId: string;
    intent: unknown;
    volumeDeletionAuthorizations: unknown;
  }) {
    const decoded = yield* decodePersistedSavedEnvironmentState(row);
    const volumeDeletionAuthorizations = yield* Schema.decodeUnknownEffect(
      destructiveVolumeReviewsSchema,
    )(row.volumeDeletionAuthorizations, strictParseOptions).pipe(
      Effect.mapError(
        () =>
          new Conflict({
            message: "Saved volume deletion authority is invalid.",
          }),
      ),
    );
    return { ...row, ...decoded, volumeDeletionAuthorizations };
  },
);

export const loadLatestEnvironmentSavedState = Effect.fn(
  "EnvironmentDesign.loadLatestEnvironmentSavedState",
)(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select(savedStateColumns)
    .from(schemaEnvironmentSavedStateSnapshot)
    .where(
      eq(schemaEnvironmentSavedStateSnapshot.environmentId, environmentId),
    )
    .orderBy(
      desc(schemaEnvironmentSavedStateSnapshot.createdAt),
      desc(schemaEnvironmentSavedStateSnapshot.id),
    )
    .limit(1);
  const row = rows[0];
  return row === undefined ? null : yield* decodeSavedState(row);
});

export const loadEnvironmentSavedIntentById = Effect.fn(
  "EnvironmentDesign.loadEnvironmentSavedIntentById",
)(function* (input: {
  environmentId: string;
  savedStateSnapshotId: string;
}) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select(savedStateColumns)
    .from(schemaEnvironmentSavedStateSnapshot)
    .where(
      and(
        eq(
          schemaEnvironmentSavedStateSnapshot.id,
          input.savedStateSnapshotId,
        ),
        eq(
          schemaEnvironmentSavedStateSnapshot.environmentId,
          input.environmentId,
        ),
      ),
    )
    .limit(1);
  const row = rows[0];
  if (row === undefined) return null;
  const intent = yield* decodePersistedSavedEnvironmentIntent(row.intent);
  const volumeDeletionAuthorizations = yield* Schema.decodeUnknownEffect(
    destructiveVolumeReviewsSchema,
  )(row.volumeDeletionAuthorizations, strictParseOptions).pipe(
    Effect.mapError(
      () =>
        new Conflict({
          message: "Saved volume deletion authority is invalid.",
        }),
    ),
  );
  return { ...row, intent, volumeDeletionAuthorizations };
});

export const listLatestEnvironmentSavedStates = Effect.fn(
  "EnvironmentDesign.listLatestEnvironmentSavedStates",
)(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .selectDistinctOn(
      [schemaEnvironmentSavedStateSnapshot.environmentId],
      savedStateColumns,
    )
    .from(schemaEnvironmentSavedStateSnapshot)
    .orderBy(
      asc(schemaEnvironmentSavedStateSnapshot.environmentId),
      desc(schemaEnvironmentSavedStateSnapshot.createdAt),
      desc(schemaEnvironmentSavedStateSnapshot.id),
    );
  return yield* Effect.forEach(rows, decodeSavedState);
});
