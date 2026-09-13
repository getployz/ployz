import { SiteAction } from '#/components/marketing/SiteAction'

interface CTASectionProps {
  title?: string
  subtitle?: string
  primaryLabel?: string
  primaryTo?: string
  secondaryLabel?: string
  secondaryHref?: string
}

export function CTASection({
  title = 'Make your next server move boring',
  subtitle,
  primaryLabel = 'Start in cloud',
  primaryTo = '/auth',
  secondaryLabel = 'Self-host free',
  secondaryHref = '/docs',
}: CTASectionProps) {
  return (
    <section className="py-20">
      <div className="mx-auto max-w-5xl px-4">
        <div className="marketing-shell overflow-hidden rounded-[2rem] border">
          <div className="relative flex flex-col gap-8 px-6 py-12 text-center md:px-10 md:py-14">
            <div className="mx-auto flex max-w-2xl flex-col gap-3">
              <h2 className="text-3xl font-semibold tracking-tight md:text-4xl">
                {title}
              </h2>
              {subtitle ? (
                <p className="text-muted-foreground">{subtitle}</p>
              ) : null}
            </div>
            <div className="flex flex-wrap items-center justify-center gap-3">
              <SiteAction to={primaryTo} size="lg">
                {primaryLabel}
              </SiteAction>
              <SiteAction to={secondaryHref} variant="outline" size="lg">
                {secondaryLabel}
              </SiteAction>
            </div>
          </div>
        </div>
      </div>
    </section>
  )
}
