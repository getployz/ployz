import { describe, expect, it } from "vitest";
import { isApiRoute } from "#/server/api-route";

type Handler = (ctx: { request: Request; params: Record<string, string> }) => Promise<Response>;
type ServerRoute = { options: { server?: { handlers?: Partial<Record<string, Handler>> } } };

const routes = import.meta.glob<{ Route: ServerRoute }>(
  ["./**/*.ts", "../mcp.ts", "../[.]well-known/**/*.ts", "!./**/-*", "!../**/-*", "!./**/*.test.ts"],
  { eager: true },
);

/** Routes that answer for themselves: the health check touches no runtime, and `/api/$` is the 404 itself. */
const OWN_ANSWER = new Set(["./health.ts", "./$.ts"]);
/** The Organization routes are wrapped once their change log has moved. */
const isOrganizationRoute = (path: string) => path.startsWith("./org/");

const request = (method: string) => new Request("http://localhost/api", { method });

describe("API routes", () => {
  it.each(Object.entries(routes))("%s is built by apiRoute or cliRoute", (path, { Route }) => {
    const handlers = Route.options.server?.handlers;
    if (handlers === undefined || OWN_ANSWER.has(path) || isOrganizationRoute(path)) return;
    expect(isApiRoute(handlers), "build its handlers with apiRoute or cliRoute").toBe(true);
  });

  it.each(Object.entries(routes))("%s refuses other methods with 405 JSON naming only what it serves", async (path, { Route }) => {
    const handlers = Route.options.server?.handlers ?? {};
    const served = Object.keys(handlers).filter((method) => method !== "ANY");
    if (served.length === 0 || path === "./$.ts") return;
    const fallback = handlers["ANY"];
    if (fallback === undefined) throw new Error("no ANY handler: other methods render the app's HTML shell");
    const other = ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"].find((method) => !served.includes(method));
    if (other === undefined) return;
    const response = await fallback({ request: request(other), params: {} });
    expect(response.status).toBe(405);
    expect(response.headers.get("content-type")).toContain("application/json");
    const allow = (response.headers.get("allow") ?? "").split(", ");
    expect(allow.filter((method) => method !== "HEAD")).toEqual(served);
    if (served.includes("GET")) expect(allow).toContain("HEAD");
  });
});
