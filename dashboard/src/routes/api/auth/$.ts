import { createFileRoute } from "@tanstack/react-router";
import { serveAuth } from "#/modules/agent/remote-mcp.server";
import { githubCallbackResponse } from "#/routes/api/github/-connect.handler";
import { apiRoute } from "#/server/api-route";

// When one GitHub App serves sign-in and install, GitHub sends a fresh install
// to the App's first Callback URL, which may be this one. Sign-in never carries
// these params, so they mark a connect callback.
function isGithubInstallCallback(url: URL) {
  return url.pathname === "/api/auth/callback/github"
    && (url.searchParams.has("installation_id") || url.searchParams.has("setup_action"));
}

export const Route = createFileRoute("/api/auth/$")({
  server: {
    handlers: apiRoute({
      GET: ({ request }) => isGithubInstallCallback(new URL(request.url)) ? githubCallbackResponse(request) : serveAuth(request),
      POST: ({ request }) => serveAuth(request),
      OPTIONS: ({ request }) => serveAuth(request),
    }),
  },
});
