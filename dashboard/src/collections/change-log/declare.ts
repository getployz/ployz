import type { ChangeSource } from "./change-log.sources";
import type { CollectionScope } from "#/collections/scope";

/** The source tables a change name reads. The first is its key table: every other one logs that table's key through a foreign key to it. */
export type ChangeSources = readonly [ChangeSource, ...ChangeSource[]];

/** What a tab does once a change names this declaration. */
export type Invalidate = (organizationSlug: string, scope: CollectionScope) => void;

/**
 * An Org Store collection a feature owns, listed in `registry.ts`. Its rows are keyed by the key its key table logs.
 * `invalidate` runs besides the collection's own refetch, for what else a change to these rows makes stale.
 */
export interface OrgCollection<Name extends string = string, Row extends object = object> {
  readonly name: Name;
  readonly sources: ChangeSources;
  readonly key: (row: Row) => string;
  readonly invalidate?: Invalidate;
}

export const defineOrgCollection = <const Name extends string, Row extends object>(collection: OrgCollection<Name, Row>) => collection;

/** A change name that is no collection: the Organization change stream names it, and `invalidate` refreshes what reads those tables. */
export interface OrgChange<Name extends string = string> {
  readonly name: Name;
  readonly sources: ChangeSources;
  readonly invalidate: Invalidate;
}

export const defineChange = <const Name extends string>(change: OrgChange<Name>) => change;
