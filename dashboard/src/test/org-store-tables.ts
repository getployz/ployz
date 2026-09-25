import type { ChangeSnapshot } from "#/collections/query-collection";
import { collectionReadInput } from "#/collections/read.contract";

/** Every Org Store table the collection read serves; fixtures seed all of them so the gate never reads the network. */
export const orgStoreTableNames = collectionReadInput.fields.table.literals;

/** The Query data an Org Store collection caches: its rows, with no change cursor yet so the next read is full. */
export function orgStoreSeed<Row>(rows: Row[]): ChangeSnapshot<Row> {
  return { rows };
}
