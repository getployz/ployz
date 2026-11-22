import { defineChange } from "#/collections/change-log/declare";
import { latestAttemptQuery } from "./latest-attempt.queries";
import { listLatestServerWebTrafficServerFn } from "./server-web-traffic.functions";

/** Each Server's latest web traffic change. The Server page's loader prefetches it. */
export const serverWebTraffic = latestAttemptQuery("server-web-traffic", listLatestServerWebTrafficServerFn);

export const serverWebTrafficChange = defineChange({
  name: "server_web_traffic",
  sources: ["server_web_traffic_attempt"],
  invalidate: (_slug, scope) => void scope.queryClient.invalidateQueries({ queryKey: serverWebTraffic.keys.all }),
});
