import { createFileRoute, notFound } from '@tanstack/react-router'
import { markdownNotFound, requestPreference } from '#/server/content-negotiation'
import { proxyMarketing } from '#/server/marketing-proxy.server'

// Every path no other route or static file claims: the marketing site's page when it has one (see
// marketing-proxy.server.ts), else the app's 404, in Markdown for a client that prefers it.
export const Route = createFileRoute('/$')({
  server: {
    handlers: {
      GET: async ({ request, next }) =>
        (await proxyMarketing(request)) ?? (requestPreference(request) === 'markdown' ? markdownNotFound() : next()),
    },
  },
  beforeLoad: () => {
    throw notFound()
  },
  // ponytail: never renders (beforeLoad throws notFound); a component lets the GET handler defer via next().
  component: () => null,
})
