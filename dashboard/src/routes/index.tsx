import { getAuthSession } from '#/auth/auth'
import { createFileRoute, redirect } from '@tanstack/react-router'
import { getAuthSession as getRequestAuthSession } from '#/auth/auth.server'
import { proxyMarketing } from '#/server/marketing-proxy.server'

// Signed in: the dashboard. Signed out: the marketing site's / (see marketing-proxy.server.ts), or sign-in
// when there is none (Self-hosted Cloud) or it fails.
export const Route = createFileRoute('/')({
  server: {
    handlers: {
      GET: async ({ request, next }) => {
        const session = await getRequestAuthSession(request.headers)
        if (session?.session && session.user) return next()
        return (await proxyMarketing(request)) ?? next()
      },
    },
  },
  beforeLoad: async () => {
    const session = await getAuthSession()
    throw redirect({ to: session?.session && session.user ? '/cloud' : '/auth', replace: true })
  },
  // ponytail: never renders (beforeLoad always redirects); a component lets the GET handler defer via next().
  component: () => null,
})
