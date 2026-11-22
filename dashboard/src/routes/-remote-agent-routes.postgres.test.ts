import { createStartHandler, defaultStreamHandler } from "@tanstack/react-start/server";
import { expect, it } from "vitest";
import { runAsSelfHostedCloud } from "#/test/config-environment";

runAsSelfHostedCloud();

const PATHS = [
  "/mcp",
  "/.well-known/oauth-protected-resource/mcp",
  "/.well-known/oauth-protected-resource",
  "/.well-known/oauth-authorization-server",
  "/.well-known/oauth-authorization-server/api/auth",
  "/.well-known/mcp-registry-auth",
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
