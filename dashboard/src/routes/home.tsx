import { createFileRoute, redirect } from '@tanstack/react-router'
import { proxyMarketing } from '#/server/marketing-proxy.server'

// The account menu's Home: the marketing site's /home for everyone (see marketing-proxy.server.ts), or the
// dashboard when there is none (Self-hosted Cloud).
export const Route = createFileRoute('/home')({
  server: {
    handlers: {
      GET: async ({ request, next }) => (await proxyMarketing(request)) ?? next(),
    },
  },
  beforeLoad: () => {
    throw redirect({ to: '/cloud', replace: true })
  },
  // ponytail: never renders (beforeLoad always redirects); a component lets the GET handler defer via next().
  component: () => null,
})
