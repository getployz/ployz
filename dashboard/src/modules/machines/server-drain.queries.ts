import { defineChange } from "#/collections/change-log/declare";
import { latestAttemptQuery } from "./latest-attempt.queries";
import { queryOptions } from "@tanstack/react-query";
import { listLatestServerDrainsServerFn, previewServerDrainServerFn } from "./server-drain.functions";

/** Each Server's latest Drain. The Server page's loader prefetches it. */
export const serverDrains = latestAttemptQuery("server-drain", listLatestServerDrainsServerFn);

/** @data-source remote: what a Drain of one Server would do with each Service its data holds there, read when the dialog opens */
export const serverDrainPreview = (organizationSlug: string, machineId: string) => queryOptions({
  queryKey: [...serverDrains.keys.all, organizationSlug, "preview", machineId] as const,
  queryFn: () => previewServerDrainServerFn({ data: { organizationSlug, machineId } }),
  staleTime: 10_000,
});

// A Drain's Volume lines read their Volume Runs, so a run's change refetches the Drains too.
export const serverDrainChange = defineChange({
  name: "server_drain",
  sources: ["server_drain_attempt", "volume_run"],
  invalidate: (_slug, scope) => void scope.queryClient.invalidateQueries({ queryKey: serverDrains.keys.all }),
});
