import { createStartHandler, defaultStreamHandler } from "@tanstack/react-start/server";
import { afterAll, expect, it } from "vitest";

// A Self-hosted Cloud is one with no Polar settings. The app runtime reads them when this file's first request builds it.
const POLAR = ["POLAR_ACCESS_TOKEN", "POLAR_SERVER", "POLAR_WEBHOOK_SECRET", "POLAR_PRODUCT_ID"];
const saved = POLAR.map((name) => [name, process.env[name]] as const);
for (const name of POLAR) delete process.env[name];
afterAll(() => {
  for (const [name, value] of saved) if (value !== undefined) process.env[name] = value;
});

const PATHS = [
  "/mcp",
  "/.well-known/oauth-protected-resource/mcp",
  "/.well-known/oauth-authorization-server",
  "/.well-known/oauth-authorization-server/api/auth",
  "/connect",
];
const METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

// The app's own request handler, so each request goes through route dispatch the way the server sends it.
const app = createStartHandler(defaultStreamHandler);

it.each(PATHS.flatMap((path) => METHODS.map((method) => [method, path] as const)))(
  "a Self-hosted Cloud answers %s %s with a 404",
  async (method, path) => {
    const response = await app(new Request(`http://localhost:3000${path}`, { method, headers: { accept: "application/json" } }));
    expect(response.status).toBe(404);
  },
  30_000,
);
