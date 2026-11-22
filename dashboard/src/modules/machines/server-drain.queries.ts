import { defineChange } from "#/collections/change-log/declare";
import { latestAttemptQuery } from "./latest-attempt.queries";
import { listLatestServerDrainsServerFn } from "./server-drain.functions";

/** Each Server's latest Drain. The Server page's loader prefetches it. */
export const serverDrains = latestAttemptQuery("server-drain", listLatestServerDrainsServerFn);

export const serverDrainChange = defineChange({
  name: "server_drain",
  sources: ["server_drain_attempt"],
  invalidate: (_slug, scope) => void scope.queryClient.invalidateQueries({ queryKey: serverDrains.keys.all }),
});
