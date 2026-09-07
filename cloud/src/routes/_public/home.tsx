import { createFileRoute } from '@tanstack/react-router'
import { buildMarketingMeta } from '#/components/marketing/meta'
import { HomePage } from '#/routes/_public/-components/home/HomePage'

export const Route = createFileRoute('/_public/home')({
  head: () => ({
    meta: buildMarketingMeta({
      title: 'Ployz — The platform experience, on your servers.',
      description:
        'Connect your repo and your servers. Ployz gives your team a clear path from preview to production without taking over your infrastructure.',
    }),
  }),
  component: HomePage,
})
