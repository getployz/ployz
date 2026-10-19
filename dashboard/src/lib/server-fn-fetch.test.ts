import { afterEach, expect, it, vi } from "vitest";
import { serverFnFetch } from "./server-fn-fetch";

afterEach(() => { vi.unstubAllGlobals(); });

const answering = (response: Response) => vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response));

it("turns an error page from in front of Cloud into Cloud's own error, not its body", async () => {
  answering(new Response(JSON.stringify({ error: "soak injected" }), { status: 500, headers: { "content-type": "application/json" } }));
  await expect(serverFnFetch("/_serverFn/x", { method: "POST" }))
    .rejects.toEqual({ _tag: "PublicError", code: "UNAVAILABLE", message: "Ployz Cloud had a problem. Try again." });

  answering(new Response("<html>502 Bad Gateway</html>", { status: 502, headers: { "content-type": "text/html" } }));
  await expect(serverFnFetch("/_serverFn/x", { method: "POST" })).rejects.toMatchObject({ code: "UNAVAILABLE" });
});

it("passes on what Cloud answered itself, its failures included", async () => {
  const failed = new Response("{}", { status: 500, headers: { "content-type": "application/json", "x-tss-serialized": "true" } });
  answering(failed);
  expect(await serverFnFetch("/_serverFn/x", { method: "POST" })).toBe(failed);

  const missing = new Response("{}", { status: 404, headers: { "content-type": "application/json" } });
  answering(missing);
  expect(await serverFnFetch("/_serverFn/x", { method: "POST" })).toBe(missing);
});
