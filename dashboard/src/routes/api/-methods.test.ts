import { describe, expect, it } from "vitest";

type Handler = (ctx: { request: Request }) => Response | Promise<Response>;
type ApiRoute = { options: { server?: { handlers?: Partial<Record<string, Handler>> } } };

const routes = import.meta.glob<{ Route: ApiRoute }>(["./**/*.ts", "!./**/-*", "!./**/*.test.ts"], { eager: true });
const METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

describe("API routes", () => {
  it.each(Object.entries(routes))("%s answers a method it doesn't take with 405 JSON", async (_, { Route }) => {
    const handlers = Route.options.server?.handlers ?? {};
    const taken = METHODS.filter((method) => handlers[method] !== undefined);
    const other = METHODS.find((method) => !taken.includes(method) && !(method === "HEAD" && taken.includes("GET")));
    if (other === undefined || taken.length === 0) return;
    const fallback = handlers["ANY"];
    if (fallback === undefined) throw new Error("no ANY handler: other methods render the app's HTML shell");
    const response = await fallback({ request: new Request("http://localhost/api", { method: other }) });
    expect(response.status).toBe(405);
    expect(response.headers.get("content-type")).toContain("application/json");
    for (const method of taken) expect(response.headers.get("allow")).toContain(method);
  });
});
