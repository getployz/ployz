import type { ReactNode } from 'react'
import { Link } from '@tanstack/react-router'
import { buttonVariants } from '#/components/ui/button-variants'

type SiteActionProps = {
  children: ReactNode
  className?: string
  href?: string
  rel?: string
  size?: 'default' | 'sm' | 'lg' | 'icon' | 'icon-sm' | 'icon-lg'
  target?: string
  to?: string
  variant?: 'default' | 'outline' | 'secondary' | 'ghost' | 'link'
}

export function SiteAction({
  children,
  className,
  href,
  rel,
  size = 'default',
  target,
  to,
  variant = 'default',
}: SiteActionProps) {
  if (to) {
    return (
      <Link
        to={to}
        className={buttonVariants({
          variant,
          size,
          className,
        })}
      >
        {children}
      </Link>
    )
  }

  if (!href) {
    return null
  }

  return (
    <a
      href={href}
      rel={rel}
      target={target}
      className={buttonVariants({
        variant,
        size,
        className,
      })}
    >
      {children}
    </a>
  )
}
