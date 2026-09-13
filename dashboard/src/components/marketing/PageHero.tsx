import type { ReactNode } from 'react'
import { Badge } from '#/components/ui/badge'

interface PageHeroProps {
  eyebrow?: string
  title: string
  subtitle?: string
  actions?: ReactNode
}

export function PageHero({ eyebrow, title, subtitle, actions }: PageHeroProps) {
  return (
    <section className="marketing-shell border-b">
      <div className="mx-auto max-w-5xl px-4 py-20 md:py-24">
        <div className="mx-auto flex max-w-4xl flex-col items-center gap-6 text-center">
          {eyebrow ? <Badge variant="secondary">{eyebrow}</Badge> : null}
          <div className="flex flex-col gap-4">
            <h1 className="text-4xl font-semibold tracking-tight text-balance md:text-6xl">
              {title}
            </h1>
            {subtitle ? (
              <p className="mx-auto max-w-2xl text-lg text-muted-foreground md:text-xl">
                {subtitle}
              </p>
            ) : null}
          </div>
          {actions ? (
            <div className="flex flex-wrap items-center justify-center gap-3">
              {actions}
            </div>
          ) : null}
        </div>
      </div>
    </section>
  )
}
