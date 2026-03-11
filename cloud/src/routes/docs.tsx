import { createFileRoute } from '@tanstack/react-router'
import { ArrowRight, BookOpen, Github, Terminal } from 'lucide-react'

export const Route = createFileRoute('/docs')({
  component: DocsPage,
})

function DocsPage() {
  return (
    <main>
      <section className="pb-16 pt-20">
        <div className="page-wrap">
          <p className="section-label mb-3">Documentation</p>
          <h1 className="mb-5 max-w-3xl text-4xl font-extrabold tracking-tight sm:text-5xl">
            Get started with{' '}
            <span className="gradient-text">Ployz</span>
          </h1>
          <p className="mb-10 max-w-2xl text-lg text-[var(--plz-text-muted)]">
            Everything you need to deploy, manage, and scale your
            infrastructure.
          </p>
        </div>
      </section>

      <section className="pb-24">
        <div className="page-wrap">
          <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
            {docSections.map((s) => (
              <a
                key={s.title}
                href={s.href}
                className="card group block p-6 text-[var(--plz-text)] no-underline"
              >
                <div
                  className="mb-4 flex h-10 w-10 items-center justify-center rounded-xl"
                  style={{ background: s.bg }}
                >
                  <s.icon size={20} style={{ color: s.color }} />
                </div>
                <h3 className="mb-2 text-base font-semibold">{s.title}</h3>
                <p className="mb-4 text-sm leading-relaxed text-[var(--plz-text-muted)]">
                  {s.desc}
                </p>
                <span className="inline-flex items-center gap-1 text-sm font-medium text-[var(--plz-accent-bright)] transition group-hover:gap-2">
                  Read more <ArrowRight size={14} />
                </span>
              </a>
            ))}
          </div>

          {/* Quick start */}
          <div className="mt-16">
            <h2 className="mb-6 text-2xl font-bold tracking-tight">
              Quick start
            </h2>
            <div className="terminal-window max-w-2xl">
              <div className="terminal-bar">
                <div className="terminal-dot bg-[#ff5f57]" />
                <div className="terminal-dot bg-[#febc2e]" />
                <div className="terminal-dot bg-[#28c840]" />
              </div>
              <div className="p-5 font-mono text-sm leading-7">
                <p className="text-[var(--plz-text-dim)]"># Install the CLI</p>
                <p>
                  <span className="text-[var(--color-plz-green)]">$</span>{' '}
                  <span className="text-[var(--plz-text-muted)]">
                    curl -fsSL https://get.ployz.dev | sh
                  </span>
                </p>
                <p>&nbsp;</p>
                <p className="text-[var(--plz-text-dim)]">
                  # Initialize a new cluster
                </p>
                <p>
                  <span className="text-[var(--color-plz-green)]">$</span>{' '}
                  <span className="text-[var(--plz-text-muted)]">
                    ployz init --name my-cluster
                  </span>
                </p>
                <p>&nbsp;</p>
                <p className="text-[var(--plz-text-dim)]">
                  # Join additional machines
                </p>
                <p>
                  <span className="text-[var(--color-plz-green)]">$</span>{' '}
                  <span className="text-[var(--plz-text-muted)]">
                    ployz join --token &lt;join-token&gt;
                  </span>
                </p>
                <p>&nbsp;</p>
                <p className="text-[var(--plz-text-dim)]"># Deploy a manifest</p>
                <p>
                  <span className="text-[var(--color-plz-green)]">$</span>{' '}
                  <span className="text-[var(--plz-text-muted)]">
                    ployz deploy manifest.toml
                  </span>
                </p>
              </div>
            </div>
          </div>
        </div>
      </section>
    </main>
  )
}

const docSections = [
  {
    icon: Terminal,
    bg: 'rgba(124,106,239,0.12)',
    color: '#a78bfa',
    title: 'CLI Reference',
    desc: 'All commands: init, join, deploy, status, logs, connect, and more.',
    href: '#',
  },
  {
    icon: BookOpen,
    bg: 'rgba(16,185,129,0.12)',
    color: '#10b981',
    title: 'Architecture Guide',
    desc: 'How the daemon, Corrosion, WireGuard, gateway, and DNS fit together.',
    href: '#',
  },
  {
    icon: Github,
    bg: 'rgba(34,211,238,0.12)',
    color: '#22d3ee',
    title: 'Deploy Manifests',
    desc: 'ServiceSpec format, placement strategies, routing, environment variables.',
    href: '#',
  },
  {
    icon: BookOpen,
    bg: 'rgba(245,158,11,0.12)',
    color: '#f59e0b',
    title: 'Canvas & Dashboard',
    desc: 'Canvas objects, edges, compilation, deploy staging, and the three-state model.',
    href: '#',
  },
  {
    icon: Terminal,
    bg: 'rgba(124,106,239,0.12)',
    color: '#a78bfa',
    title: 'PR Environments',
    desc: 'ZFS clones, data strategies, warm pools, resource quotas, and lifecycle hooks.',
    href: '#',
  },
  {
    icon: Github,
    bg: 'rgba(16,185,129,0.12)',
    color: '#10b981',
    title: 'API Reference',
    desc: 'REST API for environments, deploys, and cluster management.',
    href: '#',
  },
]
