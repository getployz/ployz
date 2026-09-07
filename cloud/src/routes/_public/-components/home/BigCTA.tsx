import { MarketingPrimaryAction } from '#/routes/_public/-components/MarketingPrimaryAction'

export function BigCTA() {
  return (
    <section
      id="cta"
      className="marketing-final-cta"
    >
      <div className="marketing-frame marketing-final-cta__inner">
        <h2>Keep the platform experience. Own where it runs.</h2>
        <p>
          Connect your first server and ship through the same workflow you can
          grow with.
        </p>
        <MarketingPrimaryAction showArrow />
      </div>
    </section>
  )
}
