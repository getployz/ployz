import "@tanstack/react-start/server-only";
import type { SQL } from "drizzle-orm";
import type { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import type { AnyPgColumn, PgTable } from "drizzle-orm/pg-core";
import type { Effect } from "effect";
import type { OrgCollection } from "./declare";
import type { Database } from "#/server/database.server";

/** The condition that keeps a read to the Organization's rows, and to the changed keys of a delta read. */
export type Scoped = (table: PgTable & { organizationId: AnyPgColumn }) => SQL | undefined;

/** How a collection reads its rows from the database: only what the browser may see, in the shape its row type has. */
export type Projection<Row extends object> = (scoped: Scoped) => Effect.Effect<Row[], EffectDrizzleQueryError, Database>;

/** A collection's projection with the tables whose changes it reads, so the read keys its delta by the first one. */
export const defineProjection = <Row extends object>(collection: OrgCollection<string, Row>, project: Projection<NoInfer<Row>>) => ({
  sources: collection.sources,
  project,
});
