import { queryOptions } from "@tanstack/react-query";
import { observePrivateAccessServerFn } from "./private-access.functions";

export const privateAccessKeys = { all: ["private-access"] as const };

export function privateAccessQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...privateAccessKeys.all, organizationSlug] as const,
    queryFn: () => observePrivateAccessServerFn({ data: { organizationSlug } }),
    staleTime: 10_000,
    refetchInterval: 15_000,
  });
}
