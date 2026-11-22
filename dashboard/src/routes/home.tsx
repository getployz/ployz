import { createFileRoute, redirect } from '@tanstack/react-router'
import { proxyMarketing } from '#/modules/marketing/marketing-proxy.server'
import { neverRenders } from '#/routes/-never-renders'

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
  component: neverRenders,
})
