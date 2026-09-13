import { useState } from 'react'
import { Link, useHydrated } from '@tanstack/react-router'
import { ArrowRightIcon } from 'lucide-react'
import { toast } from 'sonner'
import {
  getSignOutErrorMessage,
  useAuth,
  useSignOut,
} from '#/auth/auth.hooks'
import { Button } from '#/components/ui/button'
import { buttonVariants } from '#/components/ui/button-variants'
import { Spinner } from '#/components/ui/spinner'
import { Result } from 'effect'

export default function AppHeaderActions() {
  const auth = useAuth()
  const isHydrated = useHydrated()
  const signOut = useSignOut()
  const [isSigningOut, setIsSigningOut] = useState(false)

  async function handleSignOut() {
    setIsSigningOut(true)

    const result = await signOut()

    if (Result.isFailure(result)) {
      toast.error(getSignOutErrorMessage(result.failure))
    }

    setIsSigningOut(false)
  }

  return (
    <div className="flex items-center gap-2">
      {auth?.user ? (
        <>
          <Link
            to="/cloud"
            className={buttonVariants({ variant: 'ghost', size: 'sm' })}
          >
            Dashboard
          </Link>
          <Button
            size="sm"
            variant="outline"
            disabled={!isHydrated || isSigningOut}
            onClick={() => void handleSignOut()}
          >
            {isSigningOut ? <Spinner data-icon="inline-start" /> : null}
            {isSigningOut ? 'Signing out' : 'Sign out'}
          </Button>
        </>
      ) : (
        <>
          <Link
            to="/auth"
            className={buttonVariants({ variant: 'ghost', size: 'sm' })}
          >
            Sign in
          </Link>
          <Link
            to="/home"
            hash="cta"
            className={buttonVariants({ variant: 'default', size: 'sm' })}
          >
            Self-host peacefully
            <ArrowRightIcon data-icon="inline-end" />
          </Link>
        </>
      )}
    </div>
  )
}
