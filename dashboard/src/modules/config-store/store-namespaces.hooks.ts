import type { OwnedNamespace } from "@ployz/sdk";
import { useQueries, type QueryState } from "@tanstack/react-query";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { StoreResult, StoreViewOf } from "./store.contract";
import { storeViewOptions } from "./store-view.queries";

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
