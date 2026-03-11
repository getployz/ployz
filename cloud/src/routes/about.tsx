import { createFileRoute } from '@tanstack/react-router'
import { ArrowRight, Terminal } from 'lucide-react'

export const Route = createFileRoute('/about')({
  component: AboutPage,
})

function AboutPage() {
  return (
    <main>
      <section className="pb-16 pt-20">
        <div className="page-wrap">
          <p className="section-label mb-3">About</p>
          <h1 className="mb-6 max-w-3xl text-4xl font-extrabold tracking-tight sm:text-5xl">
            Infrastructure should be{' '}
            <span className="gradient-text">yours to keep</span>
          </h1>
          <div className="max-w-2xl space-y-5 text-base leading-relaxed text-[var(--plz-text-muted)] sm:text-lg">
            <p>
              Ployz started with a simple belief: you shouldn't have to choose
              between developer experience and infrastructure ownership. Most
              platforms give you great UX in exchange for lock-in. We think
              that's a false trade-off.
            </p>
            <p>
              The core daemon is open source and always will be. It manages your
              WireGuard mesh, distributed state, container deploys, and service
              routing. It runs on your machines. If it crashes, restarts, or
              upgrades &mdash; nothing in the data plane notices.
            </p>
            <p>
              The cloud dashboard is an optional layer for teams. It adds a
              visual canvas, PR environments, team management, and audit
              logging. It talks to the same daemon API as the CLI. Disconnect it
              and everything keeps running.
            </p>
            <p>
              We call this the "lens, not a brain" architecture. The dashboard
              observes and commands. The daemon decides and executes. You can
              eject anytime and take your entire stack with you as JSON
              manifests.
            </p>
          </div>

          <div className="mt-12 flex flex-wrap gap-4">
            <a href="#" className="btn-primary">
              Get early access <ArrowRight size={16} />
            </a>
            <a
              href="https://github.com/getployz/ployz"
              target="_blank"
              rel="noreferrer"
              className="btn-secondary"
            >
              <Terminal size={16} /> View source
            </a>
          </div>
        </div>
      </section>
    </main>
  )
}
