import type { Server } from "#/modules/machines/use-servers";

/** The names of the Services on a Server, each once. */
export const serviceNames = (server: Server) => [...new Set(server.services.map((service) => service.name))];

/** What runs on a Server, in the same words wherever it shows. */
export function runsHere(server: Server) {
  const names = serviceNames(server);
  if (names.length > 0) return names.join(" · ");
  return server.machine.acceptsBuilds ? "Only runs builds" : "Nothing running";
}
