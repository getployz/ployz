import { needsAttention } from "#/modules/machines/server-status";
import type { Server } from "#/modules/machines/use-servers";

/** Why an action that reaches the Server is off, or null when it can run. */
export function needsServerOnline(action: string, server: Server, stale: boolean) {
  if (stale) return `${action} needs Cloud to reach your servers.`;
  return server.status === "offline" ? `${action} needs ${server.name} online.` : null;
}

/** The other Servers that are up, and, for an offline Server, how many of its Services run on none of them. */
export function offlineCover(server: Server, servers: readonly Server[]) {
  const up = servers.filter((other) => other.machine.id !== server.machine.id && !needsAttention(other.status));
  const onlyHere = server.status === "offline"
    ? server.services.filter((service) => !up.some((other) => service.machineIds.has(other.machine.id))).length
    : 0;
  return { up, onlyHere };
}
