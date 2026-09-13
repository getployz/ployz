import { getAuthSession } from '#/auth/auth'
import { createFileRoute, redirect } from '@tanstack/react-router'
import { buildMarketingMeta } from '#/components/marketing/meta'
import { HomePage } from '#/routes/_public/-components/home/HomePage'

export const Route = createFileRoute('/_public/')({
  beforeLoad: async () => {
    const session = await getAuthSession()

    if (session?.session && session.user) {
      throw redirect({ to: '/cloud', replace: true })
    }
  },
  head: () => ({
    meta: buildMarketingMeta({
      title: 'Ployz — Your ship. Your rules.',
      description:
        'Bring your stack and your sense of adventure. Open-source deployment on servers you control. Your ship. Your rules.',
    }),
  }),
  component: HomePage,
})
