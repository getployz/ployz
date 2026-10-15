import { createFileRoute } from '@tanstack/react-router'
import { serveAuth } from '#/modules/agent/remote-mcp.server'
import { publicErrorResponse } from '#/server/public-error'
import { runAppEffect } from '#/server/run.server'

function handle(request: Request) {
  return runAppEffect(serveAuth(request), {
    signal: request.signal,
  }).catch(publicErrorResponse)
}

export const Route = createFileRoute('/api/auth/$')({
  server: {
    handlers: {
      GET: async ({ request }) => handle(request),
      POST: async ({ request }) => handle(request),
      OPTIONS: async ({ request }) => handle(request),
    },
  },
})
