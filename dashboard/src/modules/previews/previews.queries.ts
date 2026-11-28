/** @data-source remote: one repository's previews and the preview rule a start-from Environment drafts, Config Store views keyed and refetched as every other (the change stream rereads them with the Store tables behind them, `storeViewRefreshedBy`); a preview rule draft asks the Servers which can hold a preview, so it streams */
import { keepPreviousData, queryOptions, skipToken, useQuery, useSuspenseQuery, type QueryClient } from "@tanstack/react-query";
import type { ConfigView } from "@ployz/sdk";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { readStoreViewServerFn } from "#/modules/config-store/store.functions";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { queryOf, storeViewPrefix } from "#/modules/config-store/store-view.queries";
import {
  asStoreQuery, previewRuleQuery, previewRuleViewOf, previewsQuery, previewsViewOf, type PreviewRuleQuery, type PreviewsQuery,
} from "./previews.contract";

/** Reads one preview view; a busy Store is a failed read, retried, and any other refusal is the view's answer. */
async function readPreviewView<T>(organizationSlug: string, query: ReturnType<typeof previewsQuery> | ReturnType<typeof previewRuleQuery>,
  viewOf: (view: ConfigView) => T | undefined): Promise<StoreResult<T>> {
  const result = await readStoreViewServerFn({ data: { organizationSlug, query: asStoreQuery(query) } });
  if (!result.ok) {
    if (result.refusal.code === "unavailable") throw new Error(result.refusal.message);
    return result;
  }
  const view = viewOf(result.value);
  if (view === undefined) throw new Error(`The Store answered a ${query.query} query with a ${result.value.view} view.`);
  return { ok: true, value: view };
}

/** One repository's previews: its settings, how many are live, and each open pull request's outcome. */
export function previewsQueryOptions(organizationSlug: string, scope: CollectionScope, query: PreviewsQuery) {
  const asked = previewsQuery(query);
  return queryOptions({
    queryKey: [...storeViewPrefix(organizationSlug), scope.sessionId, scope.userId, asked] as const,
    staleTime: Infinity,
    queryFn: () => readPreviewView(organizationSlug, asked, previewsViewOf),
  });
}

/** What a preview rule copying `start_from` covers: its Services, secrets and the Servers that can hold a preview. */
export function previewRuleQueryOptions(organizationSlug: string, scope: CollectionScope, query: PreviewRuleQuery) {
  const asked = previewRuleQuery(query);
  return queryOptions({
    queryKey: [...storeViewPrefix(organizationSlug), scope.sessionId, scope.userId, asked] as const,
    staleTime: Infinity,
    queryFn: () => readPreviewView(organizationSlug, asked, previewRuleViewOf),
  });
}

/** One repository's previews, prefetched by the page's loader. */
export function usePreviews(organizationSlug: string, query: PreviewsQuery) {
  return useSuspenseQuery(previewsQueryOptions(organizationSlug, useCollectionScope(), query)).data;
}

/** The preview rule a start-from drafts, streamed by the Preview settings loader; null reads none. */
export function usePreviewRule(organizationSlug: string, query: PreviewRuleQuery | null) {
  const options = previewRuleQueryOptions(organizationSlug, useCollectionScope(), query ?? { project: null, repository: "", start_from: "" });
  return useQuery({ ...options, staleTime: Infinity, queryFn: query ? options.queryFn : skipToken, placeholderData: keepPreviousData });
}

/** Rereads every previews view of the Organization: a deploy or close moved what they show. */
export function invalidatePreviews(queryClient: QueryClient, organizationSlug: string) {
  return queryClient.invalidateQueries({
    queryKey: storeViewPrefix(organizationSlug),
    predicate: (query) => {
      const kind: string | undefined = queryOf(query)?.query;
      return kind === "previews";
    },
  });
}
