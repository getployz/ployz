import { getAuthSession } from '#/auth/auth'
import { Link, createFileRoute, redirect } from '@tanstack/react-router'
import { buildMarketingMeta } from '#/components/marketing/meta'
import { PloyzLogo } from '#/components/icons/ployz-logo'
import { buttonVariants } from '#/components/ui/button-variants'

export const Route = createFileRoute('/_public/')({
  beforeLoad: async () => {
    const session = await getAuthSession()

    if (session?.session && session.user) {
      throw redirect({ to: '/cloud', replace: true })
    }
  },
  head: () => ({
    meta: buildMarketingMeta({
      title: 'Ployz',
      description: 'Deploy and run applications on servers you control.',
    }),
  }),
  component: StubLander,
})

// ponytail: stub until the separate marketing site ships.
function StubLander() {
  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-6 px-4 text-center">
      <PloyzLogo />
      <p className="text-muted-foreground">
        Deploy and run applications on servers you control.
      </p>
      <Link to="/auth" className={buttonVariants()}>
        Sign in
      </Link>
    </main>
  )
}
