import { GitHubMarkIcon } from '#/components/icons/github-mark'
import { buttonVariants } from '#/components/ui/button-variants'
import { cn } from '#/lib/utils'
import { MarketingPrimaryAction } from '#/routes/_public/-components/MarketingPrimaryAction'

export function Hero() {
  return (
    <section className="benefit-hero">
      <div className="marketing-frame benefit-hero__inner">
        <p className="marketing-kicker">Ployz Cloud · open beta</p>
        <h1>The platform experience, on your servers.</h1>
        <p className="benefit-hero__lede">
          Connect your repo and your servers. Ployz takes care of the path from
          preview to production, without taking over your infrastructure.
        </p>

        <div className="marketing-actions">
          <MarketingPrimaryAction showArrow />
          <a
            href="https://github.com/getployz/ployz"
            className={cn(
              buttonVariants({ variant: 'outline', size: 'lg' }),
              'marketing-action',
            )}
          >
            <GitHubMarkIcon data-icon="inline-start" />
            View the source
          </a>
        </div>

        <p className="benefit-hero__availability">
          Managed dashboard or open-source core. Your hardware either way.
        </p>
      </div>
    </section>
  )
}
