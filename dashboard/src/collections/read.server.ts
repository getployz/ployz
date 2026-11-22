import "@tanstack/react-start/server-only";
import { and, eq, inArray, sql } from "drizzle-orm";
import type { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import type { AnyPgColumn, PgTable } from "drizzle-orm/pg-core";
import { Data, Effect } from "effect";
import { projections } from "./registry.server";
import type { CollectionRead, CollectionReadInput } from "./read.contract";
import type { Actor } from "#/modules/identity/actor";
import { changeSources } from "#/collections/change-log/change-log.sources";
import { readChangeWindow, type OrganizationChangeLogFailure } from "#/collections/change-log/change-log.server";
import { requireAccess } from "#/modules/organization/access.server";
import { Database } from "#/server/database.server";

export class CollectionReadFailure extends Data.TaggedError("CollectionReadFailure")<{
  readonly cause: unknown;
}> {
  readonly publicErrorCategory = "internal" as const;
}
export class CollectionReadDenied extends Data.TaggedError("CollectionReadDenied")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "not-found" as const;
}

const CURSOR = /^([0-9a-f-]{36})\.(\d{1,20})$/u;

export const readCollection = Effect.fn("Collections.read")(function* (
  actor: Actor,
  data: CollectionReadInput,
) {
  if (actor.userId !== data.userId) {
    return yield* new CollectionReadDenied({ message: "Collection not found." });
  }
  const { organization } = yield* requireAccess(actor, data.organizationSlug, "org:read").pipe(
    Effect.mapError((cause) => cause._tag === "NotFound" || cause._tag === "Forbidden"
      ? new CollectionReadDenied({ message: "Organization not found." })
      : new CollectionReadFailure({ cause })),
  );
  const projection = projections[data.table];
  // `keys` narrows a read to the rows its change window names, by the key its key table logs.
  const readRows = (keys?: string[]) => Effect.gen(function* () {
    const keyColumns = changeSources[projection.sources[0]].key;
    const scoped = (table: PgTable & { organizationId: AnyPgColumn }) => and(
      eq(table.organizationId, organization.id),
      keys && inArray(sql.join(keyColumns.map((column) => sql`${table}.${sql.identifier(column)}`), sql` || ':' || `), keys),
    );
    return yield* projection.project(scoped);
  });
  type Row = Effect.Success<ReturnType<typeof readRows>>[number];
  const issued = CURSOR.exec(data.since ?? "");
  const since = issued?.[1] === organization.id ? issued[2] : undefined;
  const read = Effect.gen(function* (): Effect.fn.Return<CollectionRead<Row>, EffectDrizzleQueryError | OrganizationChangeLogFailure, Database> {
    // The window is read before the rows, so the rows are at least as new as its cursor.
    const window = yield* readChangeWindow({ organizationId: organization.id, since, sourceTables: projection.sources });
    const cursor = `${organization.id}.${window.cursor}`;
    if (window.kind === "full") return { full: true, rows: yield* readRows(), cursor };
    // Deleted keys are re-read too: a key a filtered read shares with another user's row
    // (project preferences) can be deleted there and still exist here. The client drops, then upserts.
    const keys = [...new Set([...window.changed, ...window.deleted])];
    const rows = keys.length === 0 ? [] : yield* readRows(keys);
    return { full: false, rows, deleted: window.deleted, cursor };
  });
  return yield* read.pipe(Effect.mapError((cause) => new CollectionReadFailure({ cause })));
});
