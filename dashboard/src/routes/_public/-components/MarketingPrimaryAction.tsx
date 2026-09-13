import { Link } from '@tanstack/react-router'
import { ArrowRightIcon } from 'lucide-react'
import { useAuth } from '#/auth/auth.hooks'
import { buttonVariants } from '#/components/ui/button-variants'
import { cn } from '#/lib/utils'
import LoginDialog from '#/routes/_public/-components/LoginDialog'

type MarketingPrimaryActionProps = {
  showArrow?: boolean
}

export function MarketingPrimaryAction({
  showArrow = false,
}: MarketingPrimaryActionProps) {
  const auth = useAuth()
  const className = 'marketing-action marketing-action--primary'
  const content = (
    <>
      {auth?.user ? 'Dashboard' : 'Start with Ployz'}
      {showArrow ? <ArrowRightIcon data-icon="inline-end" /> : null}
    </>
  )

  if (auth?.user) {
    return (
      <Link
        to="/cloud"
        preload="intent"
        className={cn(
          buttonVariants({ variant: 'default', size: 'lg' }),
          className,
        )}
      >
        {content}
      </Link>
    )
  }

  return (
    <LoginDialog size="lg" className={className}>
      {content}
    </LoginDialog>
  )
}
