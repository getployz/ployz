/** @data-source remote: the copies deleted Volumes left on one Server, read from its Servers when its page opens */
import { queryOptions } from "@tanstack/react-query";
import { listServerLeftoversServerFn } from "./volume-run.leftovers.functions";

export const serverLeftovers = (organizationSlug: string, machineId: string) => queryOptions({
  queryKey: ["volume-leftovers", organizationSlug, machineId] as const,
  queryFn: () => listServerLeftoversServerFn({ data: { organizationSlug, machineId } }),
  staleTime: 60_000,
});
