import { getAuthSession } from '#/auth/auth'
import { createFileRoute, redirect } from '@tanstack/react-router'
import { LoginPanel } from '#/routes/_public/-components/LoginPanel'

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
    meta: [
      { title: 'Sign in - Ployz' },
      {
        name: 'description',
        content:
          'Sign in with GitHub to create a project, connect a repo or image, and start using Ployz Cloud.',
      },
    ],
  }),
  component: RouteComponent,
})

// The header and footer come from the _public layout; the panel sits between them.
function RouteComponent() {
  return (
    <main className="flex flex-1 items-center justify-center px-5 py-20 md:py-28">
      <div className="w-full max-w-md">
        <LoginPanel />
      </div>
    </main>
  )
}
