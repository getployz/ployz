import { orgChanges, orgCollections } from "./registry";
import { changeNameSchema, type ChangeName } from "./read.contract";
import type { ChangeSource } from "#/collections/change-log/change-log.sources";
import { storeTablesOf } from "#/modules/config-store/store.change-sources";

/**
 * The source tables a change stream name reads: an Org Store collection, another change a feature declares, or a
 * Config Store table family. A collection's first source is its key table: its rows are keyed by the key that table
 * logs, and every other source logs that same key through a foreign key to it.
 */
export function sourcesOf(name: ChangeName): readonly ChangeSource[] {
  const declared = [...orgCollections, ...orgChanges].find((declaration) => declaration.name === name);
  return declared?.sources ?? storeTablesOf(name);
}

export function collectionsOf(sourceTables: Iterable<string>) {
  const tables = new Set<string>(sourceTables);
  return changeNameSchema.literals.filter((name) => sourcesOf(name).some((table) => tables.has(table)));
}
