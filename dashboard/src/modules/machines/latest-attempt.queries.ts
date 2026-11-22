/** @data-source remote: each Server's latest attempt of one kind, the latest-of over history computed on the server; the change stream refetches it when an attempt's row changes */
import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";

type ListLatest<Latest> = (input: { data: { organizationSlug: string } }) => Promise<{ readonly servers: Readonly<Record<string, Latest>> }>;

/**
 * Each Server's latest Drain or web traffic change, keyed by Machine ID. `keys.all` is what the change stream
 * invalidates when an attempt's row changes.
 */
export function latestAttemptQuery<Latest>(root: string, list: ListLatest<Latest>) {
  const keys = { all: [root] as const };
  const options = (organizationSlug: string) => queryOptions({
    queryKey: [...keys.all, organizationSlug, "latest"] as const,
    queryFn: () => list({ data: { organizationSlug } }),
    staleTime: 60_000,
  });
  return { keys, options, useLatest: (organizationSlug: string) => useSuspenseQuery(options(organizationSlug)).data };
}
