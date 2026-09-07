import { createFileRoute } from '@tanstack/react-router'
import { handleAuthRequestEffect } from '#/auth/auth.server'
import { publicErrorResponse } from '#/server/public-error'
import { runAppEffect } from '#/server/run.server'

function handle(request: Request) {
  return runAppEffect(handleAuthRequestEffect(request), {
    signal: request.signal,
  }).catch(publicErrorResponse)
}

export const Route = createFileRoute('/api/auth/$')({
  server: {
    handlers: {
      GET: async ({ request }) => handle(request),
      POST: async ({ request }) => handle(request),
    },
  },
})
