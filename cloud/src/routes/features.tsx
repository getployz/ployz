import { createFileRoute } from '@tanstack/react-router'
import {
  ArrowRight,
  Boxes,
  Clock,
  Database,
  GitBranch,
  Globe,
  Layers,
  Lock,
  Network,
  RefreshCw,
  Server,
  Shield,
  Terminal,
  Zap,
} from 'lucide-react'

export const Route = createFileRoute('/features')({
  component: FeaturesPage,
})

function FeaturesPage() {
  return (
    <main>
      {/* Hero */}
      <section className="pb-16 pt-20">
        <div className="page-wrap">
          <p className="section-label mb-3">Features</p>
          <h1 className="mb-5 max-w-3xl text-4xl font-extrabold tracking-tight sm:text-5xl lg:text-6xl">
            The full picture of{' '}
            <span className="gradient-text">Ployz Cloud</span>
          </h1>
          <p className="max-w-2xl text-lg text-[var(--plz-text-muted)]">
            An open-source deployment daemon with an optional visual dashboard.
            Every feature below works with the CLI. The dashboard adds a canvas,
            team management, and PR environment automation.
          </p>
        </div>
      </section>

      {/* Canvas */}
      <section
        id="canvas"
        className="border-y border-[var(--plz-border)] bg-[var(--plz-bg-raised)] py-24"
      >
        <div className="page-wrap">
          <div className="mb-12">
            <p className="section-label mb-3">Visual Canvas</p>
            <h2 className="mb-4 text-3xl font-bold tracking-tight sm:text-4xl">
              Build your stack visually
            </h2>
            <p className="max-w-2xl text-base text-[var(--plz-text-muted)]">
              Canvas objects compile down to the same manifests the CLI uses.
              Every node type has a clear purpose, typed edges, and
              per-environment provider configuration.
            </p>
          </div>

          <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
            {canvasNodes.map((n) => (
              <div key={n.title} className="card p-5">
                <div
                  className="mb-3 flex h-9 w-9 items-center justify-center rounded-lg"
                  style={{ background: n.bg }}
                >
                  <n.icon size={18} style={{ color: n.color }} />
                </div>
                <h3 className="mb-1.5 text-sm font-semibold text-[var(--plz-text)]">
                  {n.title}
                </h3>
                <p className="text-xs leading-relaxed text-[var(--plz-text-muted)]">
                  {n.desc}
                </p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* PR Environments */}
      <section id="pr-environments" className="py-24">
        <div className="page-wrap">
          <div className="mb-12">
            <p className="section-label mb-3">PR Environments</p>
            <h2 className="mb-4 text-3xl font-bold tracking-tight sm:text-4xl">
              Every pull request gets a full stack
            </h2>
            <p className="max-w-2xl text-base text-[var(--plz-text-muted)]">
              ZFS snapshot clones, dependency-graph provisioning, automatic data
              masking. A complete environment in ~13 seconds with realistic data.
            </p>
          </div>

          <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
            {prFeatures.map((f) => (
              <div key={f.title} className="card p-5">
                <div
                  className="mb-3 flex h-9 w-9 items-center justify-center rounded-lg"
                  style={{ background: f.bg }}
                >
                  <f.icon size={18} style={{ color: f.color }} />
                </div>
                <h3 className="mb-1.5 text-sm font-semibold text-[var(--plz-text)]">
                  {f.title}
                </h3>
                <p className="text-xs leading-relaxed text-[var(--plz-text-muted)]">
                  {f.desc}
                </p>
              </div>
            ))}
          </div>

          {/* Data strategies table */}
          <div className="mt-12 overflow-x-auto">
            <table className="w-full text-left text-sm">
              <thead>
                <tr className="border-b border-[var(--plz-border)]">
                  <th className="pb-3 pr-6 font-semibold text-[var(--plz-text)]">
                    Strategy
                  </th>
                  <th className="pb-3 pr-6 font-semibold text-[var(--plz-text)]">
                    Best for
                  </th>
                  <th className="pb-3 font-semibold text-[var(--plz-text)]">
                    Speed
                  </th>
                </tr>
              </thead>
              <tbody className="text-[var(--plz-text-muted)]">
                {dataStrategies.map((s) => (
                  <tr
                    key={s.strategy}
                    className="border-b border-[var(--plz-border)]"
                  >
                    <td className="py-3 pr-6 font-mono text-xs text-[var(--plz-text)]">
                      {s.strategy}
                    </td>
                    <td className="py-3 pr-6 text-xs">{s.bestFor}</td>
                    <td className="py-3 text-xs">{s.speed}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      </section>

      {/* Deploy system */}
      <section className="border-y border-[var(--plz-border)] bg-[var(--plz-bg-raised)] py-24">
        <div className="page-wrap">
          <p className="section-label mb-3">Deploy Engine</p>
          <h2 className="mb-4 text-3xl font-bold tracking-tight sm:text-4xl">
            Atomic, distributed, recoverable
          </h2>
          <p className="mb-12 max-w-2xl text-base text-[var(--plz-text-muted)]">
            The deploy engine coordinates across machines with distributed
            locking, candidate readiness probes, and single-transaction
            commits.
          </p>

          <div className="mx-auto max-w-3xl">
            <ol className="space-y-6">
              {deploySteps.map((step, i) => (
                <li key={step.title} className="flex gap-5">
                  <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-[var(--plz-accent)]/10 text-sm font-bold text-[var(--plz-accent-bright)]">
                    {i + 1}
                  </div>
                  <div>
                    <h4 className="mb-1 text-sm font-semibold text-[var(--plz-text)]">
                      {step.title}
                    </h4>
                    <p className="text-sm leading-relaxed text-[var(--plz-text-muted)]">
                      {step.desc}
                    </p>
                  </div>
                </li>
              ))}
            </ol>
          </div>
        </div>
      </section>

      {/* Infrastructure */}
      <section className="py-24">
        <div className="page-wrap">
          <p className="section-label mb-3">Infrastructure</p>
          <h2 className="mb-12 text-3xl font-bold tracking-tight sm:text-4xl">
            Built on proven technology
          </h2>

          <div className="grid gap-5 sm:grid-cols-2">
            {infraCards.map((c) => (
              <div key={c.title} className="card p-6">
                <h3 className="mb-2 text-base font-semibold text-[var(--plz-text)]">
                  {c.title}
                </h3>
                <p className="text-sm leading-relaxed text-[var(--plz-text-muted)]">
                  {c.desc}
                </p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* CTA */}
      <section className="border-t border-[var(--plz-border)] bg-[var(--plz-bg-raised)] py-24">
        <div className="page-wrap text-center">
          <h2 className="mb-5 text-3xl font-bold tracking-tight">
            Start building on{' '}
            <span className="gradient-text">your infrastructure</span>
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

const canvasNodes = [
  {
    icon: Server,
    bg: 'rgba(124,106,239,0.12)',
    color: '#a78bfa',
    title: 'Services & Workers',
    desc: 'Compile to ServiceSpecs with ports, routes, env, and volumes. Workers are services without networking.',
  },
  {
    icon: Database,
    bg: 'rgba(16,185,129,0.12)',
    color: '#10b981',
    title: 'Databases',
    desc: 'Provision containers in dev, connect to managed providers in prod. Per-environment provider config.',
  },
  {
    icon: Shield,
    bg: 'rgba(245,158,11,0.12)',
    color: '#f59e0b',
    title: 'Config & Secrets',
    desc: 'SharedEnv, ExternalEnv (1Password, Vault, Doppler), encrypted secrets. All produce key-value pairs.',
  },
  {
    icon: Globe,
    bg: 'rgba(34,211,238,0.12)',
    color: '#22d3ee',
    title: 'Domains & Routes',
    desc: 'Domain nodes compile to RouteSpec. TCP endpoints, custom certificates, network policies.',
  },
  {
    icon: Zap,
    bg: 'rgba(124,106,239,0.12)',
    color: '#a78bfa',
    title: 'Autoscalers',
    desc: 'Control loop adjusts placement.count from metrics. Daemon only sees Replicated { count: N }.',
  },
  {
    icon: Layers,
    bg: 'rgba(16,185,129,0.12)',
    color: '#10b981',
    title: 'Init Tasks & Migrations',
    desc: 'Run-to-completion containers with dependency ordering. Migrations run after DB ready, before services.',
  },
]

const prFeatures = [
  {
    icon: Zap,
    bg: 'rgba(34,211,238,0.12)',
    color: '#22d3ee',
    title: 'ZFS Snapshot Clones',
    desc: 'Clone 100GB of staging data in 0.1 seconds. Copy-on-write means only deltas consume space.',
  },
  {
    icon: Layers,
    bg: 'rgba(124,106,239,0.12)',
    color: '#a78bfa',
    title: 'Atomic Multi-Volume',
    desc: 'zfs snapshot -r snapshots ALL child datasets atomically. Consistent point-in-time across all stores.',
  },
  {
    icon: Shield,
    bg: 'rgba(245,158,11,0.12)',
    color: '#f59e0b',
    title: 'Data Masking',
    desc: 'Hooks run after clone, before services start. PR environments never contain real PII.',
  },
  {
    icon: Clock,
    bg: 'rgba(16,185,129,0.12)',
    color: '#10b981',
    title: 'Warm Pools',
    desc: 'Pre-create N environments from latest staging snapshot. Claim one on PR open — 3 second spin-up.',
  },
  {
    icon: RefreshCw,
    bg: 'rgba(34,211,238,0.12)',
    color: '#22d3ee',
    title: 'Smart Updates',
    desc: 'On new commits: only rebuild changed images, only re-run migrations if schema changed.',
  },
  {
    icon: Lock,
    bg: 'rgba(124,106,239,0.12)',
    color: '#a78bfa',
    title: 'Resource Quotas',
    desc: 'Max concurrent, max lifetime, reduced resources, ZFS quota per clone. Autoscaler and cron disabled.',
  },
]

const dataStrategies = [
  {
    strategy: 'snapshot_clone',
    bestFor: 'Large databases, file storage',
    speed: '~0.1s',
  },
  {
    strategy: 'branch',
    bestFor: 'PlanetScale, Neon (native branching)',
    speed: '~5-15s',
  },
  {
    strategy: 'seed',
    bestFor: 'Small datasets, custom test data',
    speed: 'varies',
  },
  {
    strategy: 'empty',
    bestFor: 'No data needed, just run migrations',
    speed: '~2s',
  },
  {
    strategy: 'fresh',
    bestFor: 'Caches, queues — no state to preserve',
    speed: '~2s',
  },
  {
    strategy: 'shared',
    bestFor: 'Feature flags, auth — reuse staging',
    speed: '0s',
  },
]

const deploySteps = [
  {
    title: 'Lock',
    desc: 'Acquire namespace locks on all participant machines over TCP. Locks are tied to connection lifetime.',
  },
  {
    title: 'Discover',
    desc: 'Reconcile live container state with the store. Orphaned containers get re-registered.',
  },
  {
    title: 'Revalidate',
    desc: 'Recompute the plan while holding locks. If machines changed, abort with retry rather than deploying stale.',
  },
  {
    title: 'Create',
    desc: 'Start candidate containers and wait for readiness probes (TCP/HTTP/exec). Nothing enters routing until it passes.',
  },
  {
    title: 'Commit',
    desc: 'Single Corrosion transaction flips all head pointers, slot assignments, and deploy state atomically.',
  },
  {
    title: 'Cleanup',
    desc: 'Old instances are drained then removed. If cleanup fails, new version is live but old containers linger — recoverable, not failed.',
  },
]

const infraCards = [
  {
    title: 'WireGuard Mesh',
    desc: 'Every machine gets an overlay IPv6 address. eBPF TC classifiers intercept and redirect traffic at the kernel level. Peer-to-peer encrypted.',
  },
  {
    title: 'Corrosion (Distributed State)',
    desc: 'SQLite-based CRDT replication. All routing decisions derive from a single snapshot. Full rebuilds are cheap — no incremental consistency complexity.',
  },
  {
    title: 'Pingora Gateway',
    desc: 'HTTP/TCP reverse proxy routes by Host header to healthy instances. Round-robin load balancing, automatic failover, snapshot-based routing via double-Arc.',
  },
  {
    title: 'Disposable Control Plane',
    desc: 'The daemon can crash, upgrade, restart — workloads, WireGuard, Corrosion, gateway, and DNS all keep running. Adopt-first lifecycle on startup.',
  },
]
