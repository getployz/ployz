import { getAuthSession } from '#/auth/auth'
import { createFileRoute, redirect } from '@tanstack/react-router'
import { getAuthSession as getRequestAuthSession } from '#/auth/auth.server'
import { proxyMarketing } from '#/modules/marketing/marketing-proxy.server'
import { neverRenders } from '#/routes/-never-renders'

// Signed in: the dashboard. Signed out: the marketing site's / (see marketing-proxy.server.ts), or sign-in
// when there is none (Self-hosted Cloud) or it fails.
export const Route = createFileRoute('/')({
  server: {
    handlers: {
      GET: async ({ request, next }) => {
        const session = await getRequestAuthSession()
        if (session?.session && session.user) return next()
        return (await proxyMarketing(request)) ?? next()
      },
    },
  },
  beforeLoad: async () => {
    const session = await getAuthSession()
    throw redirect({ to: session?.session && session.user ? '/cloud' : '/auth', replace: true })
  },
  component: neverRenders,
})
