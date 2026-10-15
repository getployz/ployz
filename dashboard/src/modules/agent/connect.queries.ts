import { queryOptions } from "@tanstack/react-query";
import { createIsomorphicFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { readConnectChoicesServerFn } from "#/modules/agent/connect.functions";

/**
 * The /connect page's query exactly as better-auth signed it. The router's parsed search re-encodes repeated
 * parameters, which breaks the signature, so this reads the raw URL.
 */
export const connectPageQuery = createIsomorphicFn()
  .client(() => window.location.search.slice(1))
  .server(() => new URL(getRequest().url).search.slice(1));

/** Who is connecting, the Organizations they may pick, and the agent asking; null when signed out. */
export function connectChoicesQueryOptions(oauthQuery: string) {
  return queryOptions({
    queryKey: ["connect-choices", oauthQuery] as const,
    queryFn: () => readConnectChoicesServerFn({ data: { oauthQuery } }),
    staleTime: Infinity,
  });
}
