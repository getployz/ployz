import { getAuthSession } from '#/auth/auth'
import { createFileRoute, redirect } from '@tanstack/react-router'
import type { ReactNode } from 'react'
import { buildMarketingMeta } from '#/components/marketing/meta'
import { LoginDialog } from '#/routes/_public/-components/PublicChrome'
import { WireDefs } from '#/components/marketing/kit'
import {
  DeployTicker,
  ExitPrompt,
  GetStartedScene,
  HeroScene,
  PreviewScene,
  ReceiptScene,
  ReviewScene,
} from '#/components/marketing/scenes'
import landingCss from '#/components/marketing/landing.css?url'
import { Badge } from '#/components/ui/badge'
import { buttonVariants } from '#/components/ui/button-variants'
import { cn } from '#/lib/utils'

export const Route = createFileRoute('/_public/')({
  beforeLoad: async () => {
    const session = await getAuthSession()

    if (session?.session && session.user) {
      throw redirect({ to: '/cloud', replace: true })
    }
  },
  head: () => ({
    meta: buildMarketingMeta({
      title: 'Ployz: keep the platform, drop the bill',
      description:
        'Git push deploys and preview environments, on servers you own. Open source, and free on your own servers.',
    }),
    links: [{ rel: 'stylesheet', href: landingCss }],
  }),
  component: Lander,
})

// Written for Carol: a solo developer who already pays a hosted platform and has never had to run a
// server. It sells the #1225 flow and launches with it; only database moves, snapshots, backups and
// rollbacks are Soon. Always light, whatever the visitor's theme; colour fields use the brand's paper
// tokens (tokens.css), and Intent Pink stays on Deploy and staged changes. The header, footer and light
// scope come from the _public layout.
function Lander() {
  return (
    <main>
      <WireDefs />
      <Hero />
      <OneCommand />
      <AnyStack />
      <Platform />
      <Runs />
      <Pricing />
      <Founder />
      <Compare />
      <Questions />
      <Closer />
    </main>
  )
}

/**
 * Deploy, the page's one action: it opens sign-in over the page. `intent` is the product's solid pink
 * Deploy, used only in the hero and the closer.
 */
function DeployButton({ intent }: { intent?: boolean }) {
  return <LoginDialog className={buttonVariants({ variant: intent ? 'intent' : 'ink', size: 'lg' })}>Deploy</LoginDialog>
}

/** A small mono label, like the tags on a rack unit. */
function Tag({ children }: { children: ReactNode }) {
  return <span className="w-fit rounded border border-dashed border-current px-1.5 py-0.5 font-mono text-xs">{children}</span>
}

/** A word in a heading set on a paper block, to point at it. */
function Mark({ children }: { children: ReactNode }) {
  return <span className="rounded-lg bg-(--color-paper-strong) px-2">{children}</span>
}

function Caption({ children, className }: { children: ReactNode; className?: string }) {
  return <p className={cn('mt-4 max-w-md font-mono text-sm text-muted-foreground', className)}>{children}</p>
}

function Section({ id, className, children }: { id?: string; className?: string; children: ReactNode }) {
  return (
    <section id={id} className={cn('scroll-mt-14 px-5 py-16 md:py-24', className)}>
      <div className="mx-auto max-w-6xl">{children}</div>
    </section>
  )
}

function Title({ children }: { children: ReactNode }) {
  return <h2 className="max-w-4xl text-4xl font-semibold tracking-[-0.035em] text-balance md:text-6xl md:leading-[1.05]">{children}</h2>
}

// ---- the hero, and the dashboard on its server ---------------------------------------------------------

function Hero() {
  return (
    <Section id="top">
      <div className="flex flex-col items-center gap-6 text-center">
        <h1 className="text-5xl font-semibold tracking-[-0.045em] text-balance md:text-7xl md:leading-[1.02]">
          Keep the platform. Drop the bill.
        </h1>
        <p className="text-lg text-pretty text-muted-foreground md:text-xl">
          Git push deploys and preview environments, on servers you own.
        </p>
        <DeployButton intent />
        <p className="font-mono text-xs text-muted-foreground">Open source · Free on your own servers</p>
      </div>
      <div className="mt-14 rounded-3xl bg-(--color-paper) p-3 sm:p-8">
        <HeroScene />
      </div>
      <Caption>Your whole app on one canvas, running on a server you own.</Caption>
    </Section>
  )
}

// ---- one command, from a root prompt ----------------------------------------------------------------------

function OneCommand() {
  return (
    <Section id="start">
      <Title>
        From <Mark>root prompt</Mark> to <Mark>https</Mark> in one command.
      </Title>
      <div className="mt-10 rounded-3xl bg-(--color-paper) p-4 sm:p-8">
        <GetStartedScene />
      </div>
      <Caption>Sign in, point Ployz at your server, and it installs itself, builds your app and gives it an address.</Caption>
    </Section>
  )
}

// ---- what it runs, and where ----------------------------------------------------------------------------------

const WHERE = [
  { title: 'Start on one server', body: 'It runs your whole app: web, workers and databases.' },
  { title: 'Add servers', body: 'One command each, all on one private network.' },
  { title: 'Leave any time', body: 'Open source. If Ployz Cloud went away, your apps would keep running.' },
]

function AnyStack() {
  return (
    <Section className="dark bg-background text-foreground">
      <DeployTicker />
      <Caption className="mt-6">Builds your app without a Dockerfile, or runs any Docker image.</Caption>
      <div className="mt-16 grid grid-cols-1 gap-8 border-t border-border pt-8 md:grid-cols-3">
        {WHERE.map(({ title, body }) => (
          <div key={title} className="flex flex-col gap-2">
            <h3 className="text-lg font-semibold">{title}</h3>
            <p className="text-muted-foreground">{body}</p>
          </div>
        ))}
      </div>
    </Section>
  )
}

// ---- the platform parts ---------------------------------------------------------------------------------------

function Platform() {
  return (
    <Section id="platform">
      <Title>Everything you'd pay a platform for.</Title>
      <div className="mt-10 grid grid-cols-1 gap-6 md:grid-cols-2">
        <Feature
          field="bg-(--color-paper)"
          title="A preview environment for every pull request."
          body="Its own https address on your servers. Save its settings, and they ship with the merge."
        >
          <PreviewScene />
        </Feature>
        <Feature
          field="bg-changed-soft"
          title="See every change before it deploys."
          body="Edits in the dashboard wait in pink until you press Deploy, or discard them."
        >
          <ReviewScene />
        </Feature>
      </div>
    </Section>
  )
}

function Feature({ field, title, body, children }: { field: string; title: string; body: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-4">
      <div className={cn('flex flex-1 items-center rounded-3xl p-4 sm:p-8', field)}>{children}</div>
      <h3 className="text-lg font-semibold">{title}</h3>
      <p className="-mt-2 font-mono text-sm text-muted-foreground">{body}</p>
    </div>
  )
}

// ---- what Ployz runs for you -------------------------------------------------------------------------------------

const RUNS: { tag: string; body: ReactNode; parts: string[]; soon?: string[] }[] = [
  {
    tag: 'Start',
    body: (
      <>
        <code>ployz up</code> puts Ployz on your server, builds your app and gives it an https address.
      </>
    ),
    parts: ['One command', 'No Dockerfile needed', 'https address'],
  },
  {
    tag: 'Ship',
    body: 'Every push builds and ships. Every pull request gets its own environment.',
    parts: ['Git push deploys', 'Preview environments', 'Logs and variables'],
  },
  {
    tag: 'Grow',
    body: 'Add a server with one command, and spread your app across both.',
    parts: ['Up to 50 replicas', 'Private network', 'No Kubernetes'],
    soon: ['Database moves', 'Snapshots, backups and rollbacks'],
  },
  {
    tag: 'Agents',
    body: 'Anything you can do, your agent can do too, with output it can read.',
    parts: ['A command for every action', 'JSON output', 'The canvas follows along'],
  },
]

function Runs() {
  return (
    <Section>
      <Title>You build the app. Ployz runs the servers.</Title>
      <div className="mt-12 grid grid-cols-1 gap-10 sm:grid-cols-2 lg:grid-cols-4 lg:gap-8">
        {RUNS.map(({ tag, body, parts, soon }) => (
          <div key={tag} className="flex flex-col gap-4">
            <Tag>{tag}</Tag>
            <p>{body}</p>
            <ul className="flex flex-col font-mono text-xs">
              {parts.map((part) => (
                <li key={part} className="border-b border-border py-2">
                  {part}
                </li>
              ))}
              {soon?.map((part) => (
                <li key={part} className="flex items-center gap-2 border-b border-border py-2 text-muted-foreground">
                  {part}
                  <Badge variant="secondary">Soon</Badge>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
    </Section>
  )
}

// ---- price, proof and comparison ------------------------------------------------------------------------------------

function Pricing() {
  return (
    <Section id="pricing">
      <div className="grid grid-cols-1 items-center gap-8 rounded-3xl bg-(--color-paper) p-6 sm:p-10 md:grid-cols-[1.2fr_1fr]">
        <div className="flex flex-col items-start gap-6">
          <Tag>Pricing</Tag>
          <p className="text-3xl font-semibold tracking-[-0.03em] text-balance md:text-4xl">
            Free on your own servers. Custom domains are $9 a month.
          </p>
          <DeployButton />
          <Caption className="mt-0">Your servers are billed by your provider. We make money on custom domains, never on usage.</Caption>
        </div>
        <ReceiptScene />
      </div>
    </Section>
  )
}

function Founder() {
  return (
    <Section className="bg-(--color-paper-soft)">
      <figure className="flex flex-col gap-6">
        <blockquote className="max-w-4xl text-3xl font-medium tracking-[-0.025em] text-balance md:text-5xl md:leading-[1.1]">
          “I moved my apps to one big server and got about ten times the machine for a tenth of the cost. Then I missed
          the platform, so I built it.”
        </blockquote>
        <figcaption className="font-mono text-sm text-muted-foreground">Nick Potts, who builds Ployz</figcaption>
      </figure>
    </Section>
  )
}

// Fair to each category: where a category is mixed, it says so. The last row is Ployz's honest gap.
// Ployz comes first so it's on screen before a phone scrolls the table sideways.
const COMPARE = [
  { row: 'Where your app runs', ployz: 'Your servers', hosted: 'Their servers', selfHosted: 'Your servers' },
  { row: 'What you pay for', ployz: 'Your servers, plus $9/mo for custom domains', hosted: 'Plans and usage, often per seat', selfHosted: 'Your servers' },
  { row: 'A preview environment per pull request', ployz: 'Yes', hosted: 'Yes', selfHosted: 'Some tools' },
  { row: 'Your whole app on one canvas', ployz: 'Yes', hosted: 'Some platforms', selfHosted: 'Some tools' },
  { row: 'Dashboard changes reviewed before they deploy', ployz: 'Yes', hosted: 'Some platforms', selfHosted: 'Rarely' },
  { row: 'Adding a second server', ployz: 'One command', hosted: 'They handle it', selfHosted: 'Varies by tool' },
  { row: 'If the company disappears', ployz: 'Your apps keep running', hosted: 'You move your app', selfHosted: 'Your apps keep running' },
  { row: "Who patches the server's OS", ployz: 'You do', hosted: 'They do', selfHosted: 'You do' },
]

function Compare() {
  return (
    <Section>
      <Title>The platform, without the landlord.</Title>
      <div className="mt-10 overflow-x-auto">
        <table className="w-full min-w-2xl border-collapse text-left">
          <thead>
            <tr className="border-b border-border font-mono text-xs text-muted-foreground">
              <td className="w-1/4 py-3 pr-4" />
              <th scope="col" className="py-3 pr-4 font-medium text-foreground">
                Ployz
              </th>
              <th scope="col" className="py-3 pr-4 font-medium">
                Hosted platforms
              </th>
              <th scope="col" className="py-3 font-medium">
                Self-hosted tools
              </th>
            </tr>
          </thead>
          <tbody>
            {COMPARE.map(({ row, ployz, hosted, selfHosted }) => (
              <tr key={row} className="border-b border-border">
                <th scope="row" className="py-4 pr-4 font-medium">
                  {row}
                </th>
                <td className="py-4 pr-4 font-medium">{ployz}</td>
                <td className="py-4 pr-4 text-muted-foreground">{hosted}</td>
                <td className="py-4 text-muted-foreground">{selfHosted}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Section>
  )
}

// Carol's questions, answered honestly: what isn't built yet says so.
const QUESTIONS: { q: string; a: ReactNode }[] = [
  {
    q: 'How is this different from the platform I pay for now?',
    a: "It runs on servers you control. You still get a preview environment for every pull request, your whole app on one canvas, and dashboard changes reviewed before they deploy. It's free on your own servers, and custom domains are $9 a month. The trade: the server is yours, so its OS updates are yours too.",
  },
  {
    q: 'How is it different from other self-hosted tools?',
    a: 'One canvas for your whole app, dashboard changes reviewed before they deploy, a preview environment for every pull request, and one command to add a server to your private network. No Kubernetes, and your apps keep running if Ployz Cloud goes away.',
  },
  {
    q: 'Do I need to know Linux?',
    a: (
      <>
        You need a Linux server you can reach as root over SSH. <code>ployz up</code> does the rest.
        Ployz doesn't update the server's operating system; that stays with you.
      </>
    ),
  },
  {
    q: 'What will this cost me?',
    a: 'Ployz is free on your own servers, and $9 a month only if you want custom domains. Your provider bills you for the servers.',
  },
  {
    q: 'What happens if Ployz goes away?',
    a: 'Your servers keep running your apps. The engine and the dashboard are open source, and you can host the dashboard yourself.',
  },
  {
    q: 'What happens if my server dies?',
    a: "Services on that server stop. Replicas on your other servers keep running, but Ployz doesn't fail over for you, and a volume lives on the server it's on. Keep a copy of your data off the server; snapshots and backups are coming soon.",
  },
  {
    q: 'Are my databases backed up?',
    a: 'Not by Ployz yet: snapshots, backups and rollbacks are coming soon. Until then, keep regular dumps off the server, or keep your managed database and point your app at it with a variable.',
  },
  {
    q: 'Can I move to a bigger server later?',
    a: 'Moving a database to a bigger server without going down is coming soon. Today you can add servers and run replicas across them.',
  },
  {
    q: 'Will my app work? Do I need a Dockerfile?',
    a: "Ployz works out how to build Node, Python, Ruby, PHP, Go, Rust, Java, Elixir and more, or builds from your Dockerfile if you choose it. Any Docker image runs too. Compose files aren't supported.",
  },
  {
    q: 'Do I get https and my own domain?',
    a: 'Every first deploy gets an https address. Custom domains are $9 a month.',
  },
  {
    q: 'Can my coding agent run it?',
    a: (
      <>
        Yes. Everything the dashboard does, the <code>ployz</code> CLI does too, with JSON output.{' '}
        <code>ployz setup agent</code> teaches your agent how, and its changes show on your canvas within seconds.
      </>
    ),
  },
  {
    q: 'Is it open source?',
    a: 'Yes. The engine is Apache-2.0 and the dashboard is AGPL-3.0.',
  },
  {
    q: 'Does it scale?',
    a: "Add servers with one command, and run a service as up to 50 replicas across them, without Kubernetes. There's no autoscaling.",
  },
  {
    q: 'Where do I see logs? Can I get alerts?',
    a: "Every service's logs are in the dashboard. Ployz doesn't send alerts, so use an uptime monitor.",
  },
  {
    q: 'Is it secure?',
    a: "Your servers talk over one private network. Ployz doesn't harden or patch the operating system, so use SSH keys, a firewall and automatic updates.",
  },
  {
    q: 'Can my team use it?',
    a: 'Not yet. Ployz is built for one person today, and there are no team roles or permissions.',
  },
  {
    q: 'Which servers can I use?',
    a: 'Any Linux server with systemd that you can reach as root over SSH, on x86-64 or ARM, from any provider.',
  },
]

function Questions() {
  return (
    <Section id="questions">
      <Title>Questions</Title>
      <div className="mt-10 grid grid-cols-1 gap-x-12 border-t border-border md:grid-cols-2">
        {QUESTIONS.map(({ q, a }) => (
          <details key={q} className="group border-b border-border">
            <summary className="flex cursor-pointer list-none items-center justify-between gap-4 py-5 font-medium [&::-webkit-details-marker]:hidden">
              {q}
              <span aria-hidden className="font-mono text-muted-foreground transition-transform group-open:rotate-45">
                +
              </span>
            </summary>
            <p className="pb-5 text-muted-foreground">{a}</p>
          </details>
        ))}
      </div>
    </Section>
  )
}

function Closer() {
  return (
    <Section>
      <div className="grid grid-cols-1 items-end gap-10 md:grid-cols-[1.4fr_1fr]">
        <div className="flex flex-col items-start gap-8">
          <Title>Your servers. The whole platform.</Title>
          <DeployButton intent />
        </div>
        <ExitPrompt />
      </div>
    </Section>
  )
}
