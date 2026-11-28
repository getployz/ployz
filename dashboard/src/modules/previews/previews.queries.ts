/** @data-source remote: one repository's previews and the preview rule a start-from Environment drafts, Config Store views keyed and refetched as every other (the change stream rereads them with the Store tables behind them, `storeViewRefreshedBy`); a preview rule draft asks the Servers which can hold a preview, so it streams */
import { keepPreviousData, skipToken, useQuery, useSuspenseQuery, type QueryClient } from "@tanstack/react-query";
import type { PreviewRuleQuery, PreviewsQuery } from "@ployz/sdk";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { queryOf, storeViewOptions, storeViewPrefix } from "#/modules/config-store/store-view.queries";
import { previewRuleQuery, previewsQuery } from "./previews.contract";

/** One repository's previews: its settings, how many are live, and each open pull request's outcome. */
export function previewsQueryOptions(organizationSlug: string, scope: CollectionScope, query: PreviewsQuery) {
  return storeViewOptions(organizationSlug, scope, previewsQuery(query));
}

/** What a preview rule copying `start_from` covers: its Services, secrets and the Servers that can hold a preview. */
export function previewRuleQueryOptions(organizationSlug: string, scope: CollectionScope, query: PreviewRuleQuery) {
  return storeViewOptions(organizationSlug, scope, previewRuleQuery(query));
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
      return queryOf(query)?.query === "previews";
    },
  });
}
