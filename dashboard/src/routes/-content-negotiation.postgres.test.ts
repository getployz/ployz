import { createStartHandler, defaultStreamHandler } from "@tanstack/react-start/server";
import { afterAll, expect, it } from "vitest";

// Self-hosted (no Polar settings) keeps the remote-agent OAuth setup, which the test database lacks, out of the request.
const POLAR = ["POLAR_ACCESS_TOKEN", "POLAR_SERVER", "POLAR_WEBHOOK_SECRET", "POLAR_PRODUCT_ID"];
const saved = POLAR.map((name) => [name, process.env[name]] as const);
for (const name of POLAR) delete process.env[name];
afterAll(() => {
  for (const [name, value] of saved) if (value !== undefined) process.env[name] = value;
});

// Rendered HTML pages are out of reach here: SSR needs the client manifest, which only a vite build or dev server has.
const app = createStartHandler(defaultStreamHandler);

const send = (path: string, accept: string, method = "GET") =>
  app(new Request(`http://localhost:3000${path}`, { method, headers: { accept } }));

async function expectMarkdown(response: Response, status: number, text: string) {
  expect(response.status).toBe(status);
  expect(response.headers.get("content-type")).toBe("text/markdown; charset=utf-8");
  expect(response.headers.get("vary")).toBe("Accept");
  expect(await response.text()).toContain(text);
}

it("a page without a Markdown version refuses a Markdown-only client in Markdown", async () => {
  await expectMarkdown(await send("/cloud", "text/markdown"), 406, "/llms.txt");
}, 30_000);

it("an unknown path answers a Markdown-preferring client with a Markdown 404", async () => {
  const response = await send("/nope-xyz", "text/markdown, text/html;q=0.9");
  await expectMarkdown(response.clone(), 404, "/sitemap.xml");
  for (const link of ["/llms.txt", "/docs.md", "/agents.md"]) expect(await response.clone().text()).toContain(link);
}, 30_000);

it("the sign-in page answers a Markdown client with a summary", async () => {
  await expectMarkdown(await send("/auth", "text/markdown"), 200, "`ployz login`");
}, 30_000);

it.each(["GET", "POST", "DELETE"])("an unknown /api path answers %s with a JSON 404", async (method) => {
  const response = await send("/api/nope/deeper", "*/*", method);
  expect(response.status).toBe(404);
  expect(response.headers.get("content-type")).toBe("application/json");
  expect(await response.json()).toMatchObject({ _tag: "PublicError", code: "NOT_FOUND" });
}, 30_000);

it("a known /api route still wins over the /api catch-all", async () => {
  const response = await send("/api/health", "*/*");
  expect(response.status).toBe(200);
  expect(await response.text()).toBe("ok");
}, 30_000);
