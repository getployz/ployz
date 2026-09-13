import { getAuthSession } from '#/auth/auth'
import { createFileRoute, redirect } from '@tanstack/react-router'
import { buildMarketingMeta } from '#/components/marketing/meta'
import { LoginPanel } from '#/routes/_public/-components/LoginDialog'

export const Route = createFileRoute('/_public/auth')({
  beforeLoad: async () => {
    const session = await getAuthSession()

    if (session?.session && session.user) {
      throw redirect({
        to: '/cloud',
        replace: true,
      })
    }
  },
  head: () => ({
    meta: buildMarketingMeta({
      title: 'Sign in - Ployz',
      description:
        'Sign in with GitHub to create a project, connect a repo or image, and start using Ployz Cloud.',
    }),
  }),
  component: RouteComponent,
})

function RouteComponent() {
  return (
    <div className="flex flex-1 items-center justify-center px-4 py-12">
      <div className="w-full max-w-md">
        <LoginPanel />
      </div>
    </div>
  )
}
