import type { ConfigView, OwnedNamespace, NamedRegion } from "@ployz/sdk";
import { keepPreviousData, useQueries, type QueryState } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { StoreResult, StoreViewOf } from "#/modules/config-store/store.contract";
import { queryOf, storeViewOptions, storeViewPrefix } from "#/modules/config-store/store-view.queries";
import { toast } from "sonner";
import { isUnauthorized, toErrorMessage } from "#/lib/error-message";
import { isRequested, useStoreWriter } from "#/modules/config-store/store-write";
import { firstServerRegion } from "#/modules/machines/server-regions";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";

/**
 * The Organization's regions with their names: those the Servers' `labels` hold and those given a name. Undefined
 * until the Store first answers, and unread while `labels` is undefined; empty after a refusal or a failed read.
 */
export function useRegions(organizationSlug: string, labels: readonly string[] | undefined): readonly NamedRegion[] | undefined {
  const scope = useCollectionScope();
  const [read] = useQueries({ queries: [{
    ...storeViewOptions(organizationSlug, scope, { query: "regions", labels: [...labels ?? []] }),
    enabled: labels !== undefined,
    // The labels are in the query key; keep the names shown while the new key loads.
    placeholderData: keepPreviousData,
  }] });
  if (read.data?.ok) return read.data.value.regions;
  return read.data !== undefined || read.isError ? [] : undefined;
}

/** The Namespaces the Environments own as the Store has told: not yet, not at all, or each with where it runs. */
export type OwnedNamespaces =
  | { readonly kind: "reading" }
  | { readonly kind: "failed" }
  | { readonly kind: "read"; readonly namespaces: readonly OwnedNamespace[] };

type NamespacesRead = QueryState<StoreResult<StoreViewOf<{ readonly query: "namespaces" }>>>;

function ownedNamespaces(read: NamespacesRead | undefined): OwnedNamespaces {
  if (read?.status === "error") return { kind: "failed" };
  if (read === undefined || read.fetchStatus !== "idle" || read.isInvalidated) return { kind: "reading" };
  if (read.data?.ok) return { kind: "read", namespaces: read.data.value.namespaces };
  return read.data !== undefined ? { kind: "failed" } : { kind: "reading" };
}

/**
 * The read of the Namespaces the Organization's Environments own. `shown` is the read as this render found it, for what
 * the page says. `now` asks the read itself when called, for what a write goes by: a read can be asked again between a
 * render and the click it drew.
 */
export type OwnedNamespacesRead = { readonly shown: OwnedNamespaces; readonly now: () => OwnedNamespaces };

export function useOwnedNamespaces(organizationSlug: string): OwnedNamespacesRead {
  const scope = useCollectionScope();
  const options = storeViewOptions(organizationSlug, scope, { query: "namespaces" });
  // `now` reads the query's own state, so every change to it renders, not only the ones in a field this reads.
  useQueries({ queries: [{ ...options, notifyOnChangeProps: "all" as const }] });
  const now = () => ownedNamespaces(scope.queryClient.getQueryState(options.queryKey));
  return { shown: now(), now };
}

/**
 * The region a new Environment runs in, by the Engine's one rule: the Cluster's first Server's. `settled` waits for the
 * first observation. Only a current one pins a region, and a Machine it couldn't read counts as a Server whose region
 * isn't known, as the Engine counts it.
 */
export type NewEnvironmentRegion = { readonly settled: boolean; readonly region: string | null };

export function useNewEnvironmentRegion(organizationSlug: string): NewEnvironmentRegion {
  const runtime = useRuntimeLens(organizationSlug);
  if (runtime.status === "connecting") return { settled: false, region: null };
  if (runtime.status !== "observed") return { settled: true, region: null };
  return { settled: true, region: runtime.unreadMachines > 0 ? null : firstServerRegion(runtime.machines) };
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

/** The Store's refusals of a name that the caller shows itself, as the rest toast. */
const NAME_REFUSALS = ["invalid_argument", "conflict", "not_found"];

/**
 * Gives a region a name, shown in every cached list of regions once the Store has taken it. A refusal rejects with
 * `StoreRefused` for the caller to show; `labels` are the Servers' `region` labels, which the Store checks the key
 * against, and `creates` says the region is the one an enrollment is about to make.
 */
export function useNameRegion(organizationSlug: string) {
  const { queryClient } = useCollectionScope();
  const writer = useStoreWriter(organizationSlug);
  return async (region: NamedRegion, labels: readonly string[], creates = false) => {
    const command = { command: "name_region" as const, key: region.key, name: region.name, labels: [...labels] };
    await writer.commit(creates ? { ...command, creates } : command, NAME_REFUSALS);
    queryClient.setQueriesData<StoreResult<ConfigView>>(
      { queryKey: storeViewPrefix(organizationSlug), predicate: (query) => queryOf(query)?.query === "regions" },
      (result) => withName(result, region),
    );
  };
}

/**
 * Says a region's name wasn't saved, when its Servers' label already was: the region then shows its key until someone
 * renames it. The writer says sign-in and approval requests itself.
 */
export function nameNotSaved<Cause>(region: NamedRegion, error: Cause) {
  if (isUnauthorized(error) || (error instanceof Error && isRequested(error))) return;
  toast.error(`${region.name} is set, but its name wasn’t saved.`, {
    description: `${toErrorMessage(error, "Ployz Cloud couldn’t save it.")} It shows as ${region.key} until you rename it from Servers.`,
  });
}

function withName(result: StoreResult<ConfigView> | undefined, region: NamedRegion) {
  if (result?.ok !== true || result.value.view !== "regions") return result;
  const regions = [...result.value.regions.filter((each) => each.key !== region.key), region].sort((a, b) => a.key.localeCompare(b.key));
  return { ok: true, value: { ...result.value, regions } } as const;
}
