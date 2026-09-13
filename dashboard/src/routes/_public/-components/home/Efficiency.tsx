import { ArrowRightIcon } from 'lucide-react'

export function Efficiency() {
  return (
    <section className="marketing-frame benefit-open">
      <div>
        <h2>Managed when you want it. Open when you need it.</h2>
      </div>
      <div>
        <p>
          Use Ployz Cloud for the managed dashboard, or run the open-source core
          yourself. Either way, the workloads stay on your hardware.
        </p>
        <a href="https://github.com/getployz/ployz">
          Explore the open-source core
          <ArrowRightIcon aria-hidden="true" />
        </a>
      </div>
    </section>
  )
}
