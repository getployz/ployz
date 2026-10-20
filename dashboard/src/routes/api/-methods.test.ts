import { beforeAll, describe, expect, it, vi } from "vitest";
import * as run from "#/server/run.server";

// No route does any work here: probing a handler only shows whether it refuses its own method outright.
beforeAll(() => {
  vi.spyOn(run, "runAppEffect").mockReturnValue(new Promise(() => {}));
});

type Handler = (ctx: { request: Request; params: Record<string, string> }) => Response | Promise<Response>;
type ApiRoute = { options: { server?: { handlers?: Partial<Record<string, Handler>> } } };

const routes = import.meta.glob<{ Route: ApiRoute }>(["./**/*.ts", "!./**/-*", "!./**/*.test.ts"], { eager: true });
const METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

const request = (method: string) => new Request("http://localhost/api", { method });

/** Whether `handler` answers `method` itself, rather than refusing it with a 405. */
async function serves(handler: Handler, method: string) {
  const answer = await Promise.race([
    Promise.resolve()
      .then(() => handler({ request: request(method), params: {} }))
      .catch(() => undefined),
    new Promise<undefined>((resolve) => setTimeout(resolve, 20)),
  ]);
  return !(answer instanceof Response && answer.status === 405);
}

describe("API routes", () => {
  it.each(Object.entries(routes))("%s refuses other methods with 405 JSON naming only what it serves", async (_, { Route }) => {
    const handlers = Route.options.server?.handlers ?? {};
    const keyed = METHODS.filter((method) => handlers[method] !== undefined);
    if (keyed.length === 0) return;
    const fallback = handlers["ANY"];
    if (fallback === undefined) throw new Error("no ANY handler: other methods render the app's HTML shell");
    const other = METHODS.find((method) => !keyed.includes(method) && !(method === "HEAD" && keyed.includes("GET")));
    if (other === undefined) return;
    const response = await fallback({ request: request(other), params: {} });
    expect(response.status).toBe(405);
    expect(response.headers.get("content-type")).toContain("application/json");
    const allow = (response.headers.get("allow") ?? "").split(", ");
    for (const method of allow) {
      const handler = handlers[method] ?? (method === "HEAD" ? handlers["GET"] : undefined);
      expect(handler, `Allow names ${method}, which has no handler`).toBeDefined();
      if (handler !== undefined) expect(await serves(handler, method), `Allow names ${method}, which answers 405`).toBe(true);
    }
    for (const method of keyed) expect(allow).toContain(method);
  });
});
