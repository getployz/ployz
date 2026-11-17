import type { ConfigView, Region } from "@ployz/sdk";
import { keepPreviousData, useQueries } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { queryOf, storeViewOptions, storeViewPrefix } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";

/**
 * The Organization's regions with their names: those the Servers' `labels` hold and those given a name. Undefined
 * until the Store first answers; empty after a refusal or a failed read.
 */
export function useRegions(organizationSlug: string, labels: readonly string[]): readonly Region[] | undefined {
  const scope = useCollectionScope();
  const [read] = useQueries({ queries: [{
    ...storeViewOptions(organizationSlug, scope, { query: "regions", labels: [...labels] }),
    // The labels are in the query key; keep the names shown while the new key loads.
    placeholderData: keepPreviousData,
  }] });
  if (read.data?.ok) return read.data.value.regions;
  return read.data !== undefined || read.isError ? [] : undefined;
}

export type NewRegion =
  | { readonly kind: "checking" }
  | { readonly kind: "new"; readonly key: string }
  | { readonly kind: "refused"; readonly message: string };

const SETTLE_MS = 300;

const CHECK_FAILED = "Couldn't check this name. Try again.";

/**
 * Asks the Store, once typing pauses, what each of `names` makes as a new region: its key, or why it can't be one.
 */
export function useNewRegions(organizationSlug: string, names: readonly string[], labels: readonly string[]): (name: string) => NewRegion {
  const scope = useCollectionScope();
  const typed = [...new Set(names.map((name) => name.trim()).filter((name) => name !== ""))].sort();
  const [checked, setChecked] = useState(typed);
  useEffect(() => {
    const timer = setTimeout(() => setChecked(typed), SETTLE_MS);
    return () => clearTimeout(timer);
  }, [typed.join("\n")]);
  const reads = useQueries({
    queries: checked.map((name) => storeViewOptions(organizationSlug, scope, { query: "new_region", name, labels: [...labels] })),
  });
  return (name) => {
    const read = reads[checked.indexOf(name.trim())];
    if (read === undefined || read.isPending) return { kind: "checking" };
    if (read.data?.ok) return { kind: "new", key: read.data.value.key };
    return { kind: "refused", message: read.data?.refusal.message ?? CHECK_FAILED };
  };
}

/**
 * Gives a region a name in every cached list of regions at once; the Store's refusal rereads them.
 */
export function useNameRegion(organizationSlug: string) {
  const { queryClient } = useCollectionScope();
  const writer = useStoreWriter(organizationSlug);
  return (region: Region) => {
    queryClient.setQueriesData<StoreResult<ConfigView>>(
      { queryKey: storeViewPrefix(organizationSlug), predicate: (query) => queryOf(query)?.query === "regions" },
      (result) => withName(result, region),
    );
    return writer.commit({ command: "name_region", key: region.key, name: region.name });
  };
}

function withName(result: StoreResult<ConfigView> | undefined, region: Region) {
  if (result?.ok !== true || result.value.view !== "regions") return result;
  const regions = [...result.value.regions.filter((each) => each.key !== region.key), region].sort((a, b) => a.key.localeCompare(b.key));
  return { ok: true, value: { ...result.value, regions } } as const;
}
