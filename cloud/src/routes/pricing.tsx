import { createFileRoute } from '@tanstack/react-router'
import { ArrowRight, Check, Terminal } from 'lucide-react'

export const Route = createFileRoute('/pricing')({
  component: PricingPage,
})

function PricingPage() {
  return (
    <main>
      <section className="pb-16 pt-20">
        <div className="page-wrap text-center">
          <p className="section-label mb-3">Pricing</p>
          <h1 className="mb-5 text-4xl font-extrabold tracking-tight sm:text-5xl">
            Start free.{' '}
            <span className="gradient-text">Scale when ready.</span>
          </h1>
          <p className="mx-auto max-w-xl text-lg text-[var(--plz-text-muted)]">
            The daemon is open source and always free. The cloud dashboard adds
            team features and a visual layer. Pay only for what you use.
          </p>
        </div>
      </section>

      <section className="pb-24">
        <div className="page-wrap">
          <div className="mx-auto grid max-w-4xl gap-6 lg:grid-cols-3">
            {tiers.map((tier) => (
              <div
                key={tier.name}
                className={`card relative overflow-hidden p-6 ${tier.featured ? 'border-[var(--color-plz-accent)]/30' : ''}`}
              >
                {tier.featured && (
                  <div className="absolute right-0 top-0 rounded-bl-lg bg-[var(--plz-accent)] px-3 py-1 text-[10px] font-bold text-white">
                    POPULAR
                  </div>
                )}
                <h3 className="mb-1 text-lg font-bold text-[var(--plz-text)]">
                  {tier.name}
                </h3>
                <p className="mb-4 text-sm text-[var(--plz-text-muted)]">
                  {tier.desc}
                </p>
                <div className="mb-6">
                  <span className="text-3xl font-extrabold text-[var(--plz-text)]">
                    {tier.price}
                  </span>
                  {tier.period && (
                    <span className="text-sm text-[var(--plz-text-muted)]">
                      {tier.period}
                    </span>
                  )}
                </div>
                <a
                  href="#"
                  className={
                    tier.featured
                      ? 'btn-primary w-full justify-center !text-sm'
                      : 'btn-secondary w-full justify-center !text-sm'
                  }
                >
                  {tier.cta}
                </a>
                <ul className="mt-6 space-y-2.5">
                  {tier.features.map((f) => (
                    <li
                      key={f}
                      className="flex items-start gap-2 text-sm text-[var(--plz-text-muted)]"
                    >
                      <Check
                        size={14}
                        className="mt-0.5 shrink-0 text-[var(--color-plz-green)]"
                      />
                      {f}
                    </li>
                  ))}
                </ul>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* FAQ */}
      <section className="border-t border-[var(--plz-border)] bg-[var(--plz-bg-raised)] py-24">
        <div className="page-wrap">
          <h2 className="mb-12 text-center text-2xl font-bold tracking-tight sm:text-3xl">
            Frequently asked questions
          </h2>
          <div className="mx-auto max-w-2xl space-y-8">
            {faqs.map((faq) => (
              <div key={faq.q}>
                <h3 className="mb-2 text-base font-semibold text-[var(--plz-text)]">
                  {faq.q}
                </h3>
                <p className="text-sm leading-relaxed text-[var(--plz-text-muted)]">
                  {faq.a}
                </p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* CTA */}
      <section className="py-24">
        <div className="page-wrap text-center">
          <h2 className="mb-5 text-3xl font-bold tracking-tight">
            Ready to get started?
          </h2>
          <div className="flex flex-wrap justify-center gap-4">
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

const tiers = [
  {
    name: 'Open Source',
    desc: 'The daemon, CLI, and mesh networking',
    price: 'Free',
    period: '',
    cta: 'Get started',
    featured: false,
    features: [
      'Unlimited machines',
      'WireGuard mesh networking',
      'Atomic deploys',
      'Pingora gateway & DNS',
      'CLI deploy & management',
      'Corrosion distributed state',
      'Community support',
    ],
  },
  {
    name: 'Team',
    desc: 'Visual dashboard for growing teams',
    price: '$29',
    period: '/seat/mo',
    cta: 'Start free trial',
    featured: true,
    features: [
      'Everything in Open Source',
      'Visual canvas editor',
      'PR environments with ZFS clones',
      'Team management & RBAC',
      'Deploy queue & audit log',
      'Alerting & notifications',
      'Field-level deploy diffs',
      'Email support',
    ],
  },
  {
    name: 'Enterprise',
    desc: 'For regulated industries and scale',
    price: 'Custom',
    period: '',
    cta: 'Contact sales',
    featured: false,
    features: [
      'Everything in Team',
      'SSO / SAML',
      'Advanced audit logging',
      'Custom SLA',
      'Dedicated support engineer',
      'Multi-cluster management',
      'Warm pool automation',
      'On-call escalation',
    ],
  },
]

const faqs = [
  {
    q: 'What happens if I stop paying?',
    a: 'You lose the dashboard UI, canvas, and team features. All running services, mesh networking, and CLI access continue working. Your infrastructure never depended on us.',
  },
  {
    q: 'Do I need the cloud dashboard?',
    a: 'No. The open-source daemon and CLI are fully featured. The dashboard adds a visual canvas, team management, and PR environment automation for teams that want it.',
  },
  {
    q: 'Where does my data live?',
    a: 'On your machines. The cloud DB only stores users, teams, billing, canvas layouts, and a cache of cluster state. All operational state lives in Corrosion on your machines.',
  },
  {
    q: 'Can I export my configuration?',
    a: 'Yes. Export your canvas as DeployManifest JSON files at any time. The compiled output is identical to what the CLI accepts. Run ployz deploy -f manifest.json from CI.',
  },
  {
    q: 'How are PR environments billed?',
    a: 'PR environments run on your machines, not ours. The cloud dashboard orchestrates creation and teardown, but compute and storage are on your infrastructure.',
  },
]
