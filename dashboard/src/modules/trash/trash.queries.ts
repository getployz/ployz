/** @data-source remote: one Server's Trash, read from that Server when its page opens */
import { queryOptions } from "@tanstack/react-query";
import { listServerTrashServerFn } from "./trash.functions";

export const serverTrashKey = (organizationSlug: string, machineId: string) => ["server-trash", organizationSlug, machineId] as const;

export const serverTrash = (organizationSlug: string, machineId: string) => queryOptions({
  queryKey: serverTrashKey(organizationSlug, machineId),
  queryFn: () => listServerTrashServerFn({ data: { organizationSlug, machineId } }),
  staleTime: 60_000,
});
