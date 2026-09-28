import type { Server } from "#/modules/machines/use-servers";

/** What runs on a Server, in the same words wherever it shows. */
export function runsHere(server: Server) {
  const names = [...new Set(server.services.map((service) => service.name))];
  if (names.length > 0) return names.join(" · ");
  return server.machine.acceptsBuilds ? "Only runs builds" : "Nothing running";
}
