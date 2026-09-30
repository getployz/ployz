import { createFileRoute } from '@tanstack/react-router'
import { Lander, landerHead } from '#/routes/_public/-components/Lander'

// The lander for everyone, signed in or not: the account menu's Home. / sends signed-in visitors on.
export const Route = createFileRoute('/_public/home')({
  head: landerHead,
  component: Lander,
})
