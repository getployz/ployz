import type { AnyRouter } from "@tanstack/react-router";
import type { QueryClient } from "@tanstack/react-query";
import { endRequestCollections } from "./query-collection";

/**
 * The DB integration ends its collections when render finishes, but a HEAD request's SSR ends first: its body is
 * dropped. This SSR cleanup ends them, and must be set up before the Query integration, whose cleanup clears the
 * QueryClient under them.
 */
export function endCollectionsOnSsrCleanup(router: AnyRouter, queryClient: QueryClient) {
  router.serverSsrLifecycle = {
    ...router.serverSsrLifecycle,
    onServerSsrAttach: [...router.serverSsrLifecycle?.onServerSsrAttach ?? [],
      (serverSsr) => serverSsr.onCleanup(() => endRequestCollections(queryClient))],
  };
}
