import { collectionReadInput } from "#/collections/read.contract";

/** Every Org Store table the collection read serves; fixtures seed all of them so the gate never reads the network. */
export const orgStoreTableNames = collectionReadInput.fields.table.literals.filter((table) => table !== "github_repository_cache");
