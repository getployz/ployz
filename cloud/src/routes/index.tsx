import { createFileRoute, Link } from '@tanstack/react-router'
import {
  ArrowRight,
  Boxes,
  GitBranch,
  Globe,
  Layers,
  LogOut,
  Network,
  Shield,
  Terminal,
  Zap,
} from 'lucide-react'

export const Route = createFileRoute('/')({
  component: HomePage,
})

function HomePage() {
  return (
    <main>
      {/* Hero */}
      <section className="relative overflow-hidden pb-24 pt-20 sm:pt-28">
        {/* Background glows */}
        <div className="pointer-events-none absolute inset-0">
          <div className="absolute -left-[20%] top-0 h-[600px] w-[600px] rounded-full bg-[radial-gradient(circle,var(--plz-hero-glow-a),transparent_70%)]" />
          <div className="absolute -right-[10%] top-[10%] h-[500px] w-[500px] rounded-full bg-[radial-gradient(circle,var(--plz-hero-glow-b),transparent_70%)]" />
        </div>

        <div className="page-wrap relative">
          <div className="rise-in mb-6 inline-flex items-center gap-2 rounded-full border border-[var(--plz-border-bright)] bg-[var(--plz-bg-card)] px-4 py-1.5">
            <span className="h-2 w-2 rounded-full bg-[var(--color-plz-green)]" />
            <span className="text-xs font-medium text-[var(--plz-text-muted)]">
              Now in early access
            </span>
          </div>

          <h1 className="rise-in mb-6 max-w-4xl text-4xl font-extrabold leading-[1.08] tracking-tight sm:text-6xl lg:text-7xl">
            Deploy infrastructure{' '}
            <span className="gradient-text">that stays yours</span>
          </h1>

          <p
            className="rise-in mb-10 max-w-2xl text-lg leading-relaxed text-[var(--plz-text-muted)] sm:text-xl"
            style={{ animationDelay: '80ms' }}
          >
            A visual dashboard over your own machines. Canvas-based deploys,
            instant PR environments with ZFS clones, and zero lock-in. The
            daemon is the brain. The dashboard is a lens. Eject anytime &mdash;
            keep everything running.
          </p>

          <div
            className="rise-in flex flex-wrap gap-4"
            style={{ animationDelay: '160ms' }}
          >
            <a href="#" className="btn-primary">
              Start deploying <ArrowRight size={16} />
            </a>
            <a
              href="https://github.com/getployz/ployz"
              target="_blank"
              rel="noreferrer"
              className="btn-secondary"
            >
              <Terminal size={16} /> View on GitHub
            </a>
          </div>

          {/* Terminal demo */}
          <div
            className="rise-in mt-16 max-w-3xl"
            style={{ animationDelay: '240ms' }}
          >
            <div className="terminal-window">
              <div className="terminal-bar">
                <div className="terminal-dot bg-[#ff5f57]" />
                <div className="terminal-dot bg-[#febc2e]" />
                <div className="terminal-dot bg-[#28c840]" />
                <span className="ml-3 text-xs text-[var(--plz-text-dim)]">
                  terminal
                </span>
              </div>
              <div className="p-5 font-mono text-sm leading-7">
                <p>
                  <span className="text-[var(--color-plz-green)]">$</span>{' '}
                  <span className="text-[var(--plz-text-muted)]">
                    ployz deploy manifest.toml
                  </span>
                </p>
                <p className="text-[var(--plz-text-dim)]">
                  Planning deploy for namespace &quot;production&quot;...
                </p>
                <p className="text-[var(--plz-text-dim)]">
                  &nbsp; api &nbsp;&nbsp;&nbsp; image:v1 → image:v2
                  &nbsp;&nbsp; 3 replicas on machines A, B, C
                </p>
                <p className="text-[var(--plz-text-dim)]">
                  &nbsp; worker &nbsp; (no changes)
                </p>
                <p className="text-[var(--plz-text-dim)]">
                  &nbsp; redis &nbsp;&nbsp; (no changes)
                </p>
                <p>&nbsp;</p>
                <p>
                  <span className="text-[var(--color-plz-green)]">$</span>{' '}
                  <span className="text-[var(--plz-text-muted)]">
                    ployz apply
                  </span>
                </p>
                <p className="text-[var(--color-plz-green)]">
                  ✓ Locked namespace on 3 machines
                </p>
                <p className="text-[var(--color-plz-green)]">
                  ✓ Started candidates, readiness passed
                </p>
                <p className="text-[var(--color-plz-green)]">
                  ✓ Committed atomically — traffic switched
                </p>
                <p className="text-[var(--plz-text-dim)]">
                  Deploy complete in 8.2s
                </p>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* Logos / trust bar */}
      <section className="border-y border-[var(--plz-border)] py-10">
        <div className="page-wrap text-center">
          <p className="section-label mb-6">
            Built on battle-tested foundations
          </p>
          <div className="flex flex-wrap items-center justify-center gap-x-12 gap-y-4 text-[var(--plz-text-dim)]">
            {['WireGuard', 'Corrosion', 'Pingora', 'ZFS', 'eBPF', 'Docker'].map(
              (name) => (
                <span key={name} className="text-sm font-semibold tracking-wide">
                  {name}
                </span>
              ),
            )}
          </div>
        </div>
      </section>

      {/* Features grid */}
      <section className="py-24">
        <div className="page-wrap">
          <p className="section-label mb-3">Why Ployz Cloud</p>
          <h2 className="mb-4 max-w-2xl text-3xl font-bold tracking-tight sm:text-4xl">
            Everything you need to ship,{' '}
            <span className="gradient-text">nothing you can't leave</span>
          </h2>
          <p className="mb-14 max-w-2xl text-base text-[var(--plz-text-muted)] sm:text-lg">
            The open-source daemon runs your infrastructure. The cloud dashboard
            adds a visual layer for teams. Disconnect anytime and everything
            keeps running.
          </p>

          <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
            {features.map((f, i) => (
              <article
                key={f.title}
                className="card rise-in p-6"
                style={{ animationDelay: `${i * 60}ms` }}
              >
                <div
                  className="mb-4 flex h-10 w-10 items-center justify-center rounded-xl"
                  style={{ background: f.iconBg }}
                >
                  <f.icon size={20} style={{ color: f.iconColor }} />
                </div>
                <h3 className="mb-2 text-base font-semibold text-[var(--plz-text)]">
                  {f.title}
                </h3>
                <p className="text-sm leading-relaxed text-[var(--plz-text-muted)]">
                  {f.desc}
                </p>
              </article>
            ))}
          </div>
        </div>
      </section>

      {/* Canvas preview section */}
      <section className="border-y border-[var(--plz-border)] bg-[var(--plz-bg-raised)] py-24">
        <div className="page-wrap">
          <div className="grid items-center gap-16 lg:grid-cols-2">
            <div>
              <p className="section-label mb-3">Visual Canvas</p>
              <h2 className="mb-5 text-3xl font-bold tracking-tight sm:text-4xl">
                Drag, connect, deploy
              </h2>
              <p className="mb-6 text-base leading-relaxed text-[var(--plz-text-muted)] sm:text-lg">
                Canvas objects compile down to the same manifests the CLI uses.
                Connect services to databases, shared environments, autoscalers,
                and more. Every connection is a typed edge. Every deploy is a
                diff.
              </p>
              <ul className="space-y-3 text-sm text-[var(--plz-text-muted)]">
                {[
                  'Services, workers, cron jobs, migrations',
                  'Databases with per-environment providers',
                  'Shared env, secrets, config files',
                  'Autoscalers, health checks, resource profiles',
                ].map((item) => (
                  <li key={item} className="flex items-start gap-2.5">
                    <span className="mt-0.5 text-[var(--color-plz-green)]">
                      ✓
                    </span>
                    {item}
                  </li>
                ))}
              </ul>
              <div className="mt-8">
                <Link to="/features" className="btn-primary !text-sm">
                  Explore features <ArrowRight size={14} />
                </Link>
              </div>
            </div>

            {/* Canvas mockup */}
            <div className="grid-bg relative rounded-2xl border border-[var(--plz-border)] p-8">
              <div className="grid gap-4">
                <div className="canvas-node canvas-node-purple">
                  <div className="mb-1 flex items-center gap-2">
                    <Boxes size={14} className="text-[var(--color-plz-accent)]" />
                    <span className="text-xs font-semibold text-[var(--plz-text)]">
                      api
                    </span>
                    <span className="ml-auto rounded bg-[var(--color-plz-green)]/10 px-1.5 py-0.5 text-[10px] font-medium text-[var(--color-plz-green)]">
                      3 replicas
                    </span>
                  </div>
                  <p className="text-xs text-[var(--plz-text-dim)]">
                    image: api:v2 &middot; ports: 8080
                  </p>
                </div>

                <div className="grid grid-cols-2 gap-4">
                  <div className="canvas-node canvas-node-green">
                    <div className="mb-1 flex items-center gap-2">
                      <Layers
                        size={14}
                        className="text-[var(--color-plz-green)]"
                      />
                      <span className="text-xs font-semibold text-[var(--plz-text)]">
                        postgres
                      </span>
                    </div>
                    <p className="text-xs text-[var(--plz-text-dim)]">
                      DATABASE_URL injected
                    </p>
                  </div>
                  <div className="canvas-node canvas-node-cyan">
                    <div className="mb-1 flex items-center gap-2">
                      <Zap
                        size={14}
                        className="text-[var(--color-plz-cyan)]"
                      />
                      <span className="text-xs font-semibold text-[var(--plz-text)]">
                        redis
                      </span>
                    </div>
                    <p className="text-xs text-[var(--plz-text-dim)]">
                      REDIS_URL injected
                    </p>
                  </div>
                </div>

                <div className="canvas-node canvas-node-orange">
                  <div className="mb-1 flex items-center gap-2">
                    <Shield
                      size={14}
                      className="text-[var(--color-plz-orange)]"
                    />
                    <span className="text-xs font-semibold text-[var(--plz-text)]">
                      shared-env: secrets
                    </span>
                  </div>
                  <p className="text-xs text-[var(--plz-text-dim)]">
                    API_KEY, JWT_SECRET → api, worker
                  </p>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* PR Environments */}
      <section className="py-24">
        <div className="page-wrap">
          <div className="grid items-center gap-16 lg:grid-cols-2">
            <div className="order-2 lg:order-1">
              <div className="terminal-window">
                <div className="terminal-bar">
                  <div className="terminal-dot bg-[#ff5f57]" />
                  <div className="terminal-dot bg-[#febc2e]" />
                  <div className="terminal-dot bg-[#28c840]" />
                  <span className="ml-3 text-xs text-[var(--plz-text-dim)]">
                    PR #142 environment
                  </span>
                </div>
                <div className="p-5 font-mono text-xs leading-6">
                  <p className="text-[var(--plz-text-dim)]">
                    Phase 1: Infrastructure (parallel)
                  </p>
                  <p>
                    <span className="text-[var(--color-plz-green)]">✓</span>{' '}
                    zfs clone staging-postgres → pr-postgres{' '}
                    <span className="text-[var(--plz-text-dim)]">0.1s</span>
                  </p>
                  <p>
                    <span className="text-[var(--color-plz-green)]">✓</span>{' '}
                    zfs clone staging-minio → pr-minio{' '}
                    <span className="text-[var(--plz-text-dim)]">0.1s</span>
                  </p>
                  <p>
                    <span className="text-[var(--color-plz-green)]">✓</span>{' '}
                    Start redis (fresh){' '}
                    <span className="text-[var(--plz-text-dim)]">2s</span>
                  </p>
                  <p className="mt-2 text-[var(--plz-text-dim)]">
                    Phase 2: Data services
                  </p>
                  <p>
                    <span className="text-[var(--color-plz-green)]">✓</span>{' '}
                    postgres ready on cloned volume{' '}
                    <span className="text-[var(--plz-text-dim)]">3s</span>
                  </p>
                  <p className="mt-2 text-[var(--plz-text-dim)]">
                    Phase 3: Migrations
                  </p>
                  <p>
                    <span className="text-[var(--color-plz-green)]">✓</span>{' '}
                    Run delta migrations{' '}
                    <span className="text-[var(--plz-text-dim)]">2s</span>
                  </p>
                  <p className="mt-2 text-[var(--plz-text-dim)]">
                    Phase 4: Application
                  </p>
                  <p>
                    <span className="text-[var(--color-plz-green)]">✓</span>{' '}
                    api, worker, frontend ready{' '}
                    <span className="text-[var(--plz-text-dim)]">5s</span>
                  </p>
                  <p className="mt-2 text-[var(--plz-text-dim)]">
                    Phase 5: Validation
                  </p>
                  <p>
                    <span className="text-[var(--color-plz-green)]">✓</span>{' '}
                    Smoke tests passed{' '}
                    <span className="text-[var(--plz-text-dim)]">3s</span>
                  </p>
                  <p className="mt-3 text-[var(--color-plz-green)]">
                    Ready in 13s — pr-142.preview.ployz.dev
                  </p>
                </div>
              </div>
            </div>

            <div className="order-1 lg:order-2">
              <p className="section-label mb-3">PR Environments</p>
              <h2 className="mb-5 text-3xl font-bold tracking-tight sm:text-4xl">
                Full environments in{' '}
                <span className="gradient-text-green">13 seconds</span>
              </h2>
              <p className="mb-6 text-base leading-relaxed text-[var(--plz-text-muted)] sm:text-lg">
                ZFS snapshot clones give every pull request a complete copy of
                staging data in milliseconds. Copy-on-write means only deltas
                consume space. Databases, object stores, everything &mdash;
                cloned atomically.
              </p>
              <ul className="space-y-3 text-sm text-[var(--plz-text-muted)]">
                {[
                  'ZFS clone: 100GB database in 0.1 seconds',
                  'Atomic multi-volume snapshots across all data stores',
                  'Auto data masking — PR environments never contain real PII',
                  'Warm pools for sub-3-second spin-up',
                ].map((item) => (
                  <li key={item} className="flex items-start gap-2.5">
                    <span className="mt-0.5 text-[var(--color-plz-green)]">
                      ✓
                    </span>
                    {item}
                  </li>
                ))}
              </ul>
            </div>
          </div>
        </div>
      </section>

      {/* Architecture diagram */}
      <section className="border-y border-[var(--plz-border)] bg-[var(--plz-bg-raised)] py-24">
        <div className="page-wrap text-center">
          <p className="section-label mb-3">Architecture</p>
          <h2 className="mx-auto mb-5 max-w-2xl text-3xl font-bold tracking-tight sm:text-4xl">
            The daemon is the brain. The dashboard is a lens.
          </h2>
          <p className="mx-auto mb-14 max-w-2xl text-base text-[var(--plz-text-muted)] sm:text-lg">
            Your infrastructure runs on an open-source daemon with Corrosion as
            the source of truth. The cloud dashboard connects to the same API as
            the CLI. Disconnect it and nothing changes.
          </p>

          <div className="mx-auto max-w-3xl">
            <div className="grid-bg rounded-2xl border border-[var(--plz-border)] p-8">
              <div className="grid grid-cols-3 gap-6">
                <div className="card p-4 text-center">
                  <Terminal
                    size={24}
                    className="mx-auto mb-2 text-[var(--color-plz-green)]"
                  />
                  <p className="text-sm font-semibold text-[var(--plz-text)]">
                    CLI
                  </p>
                </div>
                <div className="card border-[var(--color-plz-accent)]/30 p-4 text-center">
                  <Network
                    size={24}
                    className="mx-auto mb-2 text-[var(--color-plz-accent)]"
                  />
                  <p className="text-sm font-semibold text-[var(--plz-text)]">
                    Daemon API
                  </p>
                  <p className="mt-1 text-[10px] text-[var(--plz-text-dim)]">
                    source of truth
                  </p>
                </div>
                <div className="card p-4 text-center">
                  <Globe
                    size={24}
                    className="mx-auto mb-2 text-[var(--color-plz-cyan)]"
                  />
                  <p className="text-sm font-semibold text-[var(--plz-text)]">
                    Dashboard
                  </p>
                </div>
              </div>

              <div className="my-6 flex items-center justify-center gap-3 text-xs text-[var(--plz-text-dim)]">
                <span>same API</span>
                <span>→</span>
                <span>same operations</span>
                <span>→</span>
                <span>zero divergence</span>
              </div>

              <div className="grid grid-cols-2 gap-6">
                <div className="card p-4 text-center">
                  <p className="text-sm font-semibold text-[var(--plz-text)]">
                    Corrosion
                  </p>
                  <p className="mt-1 text-[10px] text-[var(--plz-text-dim)]">
                    distributed state, survives eject
                  </p>
                </div>
                <div className="card p-4 text-center">
                  <p className="text-sm font-semibold text-[var(--plz-text)]">
                    Cloud DB
                  </p>
                  <p className="mt-1 text-[10px] text-[var(--plz-text-dim)]">
                    users, teams, canvas, billing
                  </p>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* Eject section */}
      <section className="py-24">
        <div className="page-wrap text-center">
          <p className="section-label mb-3">Zero lock-in</p>
          <h2 className="mx-auto mb-5 max-w-2xl text-3xl font-bold tracking-tight sm:text-4xl">
            Eject anytime.{' '}
            <span className="gradient-text">Keep everything.</span>
          </h2>
          <p className="mx-auto mb-12 max-w-2xl text-base text-[var(--plz-text-muted)] sm:text-lg">
            Export your canvas as manifest JSON files. Run{' '}
            <code>ployz deploy -f manifest.json</code> from CI. The compiled
            output is identical to what the dashboard produces.
          </p>

          <div className="mx-auto grid max-w-3xl gap-5 sm:grid-cols-2">
            <div className="card p-6 text-left">
              <h3 className="mb-3 flex items-center gap-2 text-base font-semibold text-[var(--color-plz-green)]">
                <span>✓</span> What you keep
              </h3>
              <ul className="space-y-2 text-sm text-[var(--plz-text-muted)]">
                <li>All running services (unchanged)</li>
                <li>Full Corrosion state (specs, slots, machines)</li>
                <li>CLI access to everything</li>
                <li>WireGuard mesh (unchanged)</li>
              </ul>
            </div>
            <div className="card p-6 text-left">
              <h3 className="mb-3 flex items-center gap-2 text-base font-semibold text-[var(--plz-text-dim)]">
                <LogOut size={16} /> What goes away
              </h3>
              <ul className="space-y-2 text-sm text-[var(--plz-text-dim)]">
                <li>Dashboard UI</li>
                <li>Canvas objects and visual layout</li>
                <li>Team/RBAC management</li>
                <li>The cloud agent process</li>
              </ul>
            </div>
          </div>
        </div>
      </section>

      {/* CTA */}
      <section className="border-t border-[var(--plz-border)] bg-[var(--plz-bg-raised)] py-24">
        <div className="page-wrap text-center">
          <h2 className="mb-5 text-3xl font-bold tracking-tight sm:text-4xl">
            Ready to deploy on{' '}
            <span className="gradient-text">your machines</span>?
          </h2>
          <p className="mx-auto mb-10 max-w-xl text-base text-[var(--plz-text-muted)] sm:text-lg">
            Start with the open-source CLI. Add the cloud dashboard when your
            team needs it. Leave whenever you want.
          </p>
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
              <Terminal size={16} /> Star on GitHub
            </a>
          </div>
        </div>
      </section>
    </main>
  )
}

const features = [
  {
    icon: Boxes,
    iconBg: 'rgba(124, 106, 239, 0.12)',
    iconColor: '#a78bfa',
    title: 'Visual Canvas',
    desc: 'Drag services, databases, and config nodes onto a canvas. Connect them with typed edges. Compile to deploy manifests.',
  },
  {
    icon: GitBranch,
    iconBg: 'rgba(16, 185, 129, 0.12)',
    iconColor: '#10b981',
    title: 'Instant PR Environments',
    desc: 'ZFS snapshot clones give every PR a full copy of staging data in milliseconds. Atomic, copy-on-write, auto-destroyed.',
  },
  {
    icon: Zap,
    iconBg: 'rgba(34, 211, 238, 0.12)',
    iconColor: '#22d3ee',
    title: 'Atomic Deploys',
    desc: 'A single Corrosion transaction flips all routing pointers at once. No half-deployed state. Rollback is instant.',
  },
  {
    icon: Network,
    iconBg: 'rgba(245, 158, 11, 0.12)',
    iconColor: '#f59e0b',
    title: 'WireGuard Mesh',
    desc: 'Every machine gets an overlay IP. Services discover each other by name. Traffic flows encrypted, peer-to-peer.',
  },
  {
    icon: LogOut,
    iconBg: 'rgba(124, 106, 239, 0.12)',
    iconColor: '#a78bfa',
    title: 'Clean Eject',
    desc: 'Export canvas as manifest JSON. Run from CI. The daemon never depended on the dashboard. Nothing changes.',
  },
  {
    icon: Shield,
    iconBg: 'rgba(16, 185, 129, 0.12)',
    iconColor: '#10b981',
    title: 'Disposable Control Plane',
    desc: 'The daemon can crash, upgrade, restart — and nothing in the data plane notices. WireGuard stays up. Containers keep running.',
  },
]
