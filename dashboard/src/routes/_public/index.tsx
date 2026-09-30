import { getAuthSession } from '#/auth/auth'
import { createFileRoute, redirect } from '@tanstack/react-router'
import { Lander, landerHead } from '#/routes/_public/-components/Lander'

// The lander for signed-out visitors; signed-in ones go straight on to their dashboard. /home shows it to
// everyone.
export const Route = createFileRoute('/_public/')({
  beforeLoad: async () => {
    const session = await getAuthSession()

    if (session?.session && session.user) {
      throw redirect({ to: '/cloud', replace: true })
    }
  },
  head: landerHead,
  component: Lander,
})
