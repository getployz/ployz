import { createFileRoute } from '@tanstack/react-router'
import { serveAuth } from '#/modules/agent/remote-mcp.server'
import { handleGithubCallback } from '#/routes/api/github/-connect.handler'
import { publicErrorResponse, methodNotAllowed } from '#/server/public-error'
import { runAppEffect } from '#/server/run.server'

function handle(request: Request) {
  return runAppEffect(serveAuth(request), {
    signal: request.signal,
  }).catch(publicErrorResponse)
}

// When one GitHub App serves sign-in and install, GitHub sends a fresh install
// to the App's first Callback URL, which may be this one. Sign-in never carries
// these params, so they mark a connect callback.
function isGithubInstallCallback(url: URL) {
  return url.pathname === '/api/auth/callback/github'
    && (url.searchParams.has('installation_id') || url.searchParams.has('setup_action'))
}

export const Route = createFileRoute('/api/auth/$')({
  server: {
    handlers: {
      GET: async ({ request }) => isGithubInstallCallback(new URL(request.url))
        ? handleGithubCallback(request)
        : handle(request),
      POST: async ({ request }) => handle(request),
      OPTIONS: async ({ request }) => handle(request),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
})
