import { createFileRoute } from '@tanstack/react-router'
import { CloudIcon, GitBranchIcon, ServerIcon, TerminalIcon } from 'lucide-react'
import {
  dashboardRepoHref,
  runtimeRepoHref,
} from '#/components/marketing/links'
import { buildMarketingMeta } from '#/components/marketing/meta'
import { SiteAction } from '#/components/marketing/SiteAction'
import { buttonVariants } from '#/components/ui/button-variants'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '#/components/ui/card'
import { Separator } from '#/components/ui/separator'
import { PageHero } from '#/components/marketing/PageHero'
import { CodeBlock } from '#/components/marketing/CodeBlock'
import { Link } from '@tanstack/react-router'

export const Route = createFileRoute('/_public/docs')({
  head: () => ({
    meta: buildMarketingMeta({
      title: 'Docs - Ployz',
      description:
        'Start with one machine, add more when you need to, and use the same Ployz model across cloud and self-hosting.',
    }),
  }),
  component: DocsPage,
})

const cliCommands = [
  {
    cmd: 'npx ployzctl daemon install',
    description: 'Install the Ployz daemon on the current server',
  },
  {
    cmd: 'ployzctl mesh init',
    description: 'Initialize the overlay mesh on this machine',
  },
  {
    cmd: 'curl -fsSL https://ployz.sh | sh && sudo ployz host bootstrap cloud --cloud-host ployz.dev',
    description: 'Enroll another machine with the Cloud bootstrap command',
  },
  {
    cmd: 'ployzctl deploy preview -f manifest.json',
    description: 'Preview a deploy before you apply it',
  },
  {
    cmd: 'ployzctl deploy -f manifest.json',
    description: 'Apply the manifest when the plan looks right',
  },
]

function DocsPage() {
  return (
    <main>
      <PageHero
        eyebrow="Docs"
        title="Start on one machine. Grow when you need to."
        subtitle="Ployz is built for operators who want one model across cloud and self-hosting without jumping straight into Kubernetes."
      />

      <section className="py-20">
        <div className="mx-auto max-w-4xl px-4">
          <div className="grid gap-6 md:grid-cols-2">
            <Card>
              <CardHeader>
                <div className="flex items-center gap-2">
                  <CloudIcon className="size-4 text-primary" />
                  <CardTitle className="text-base">Use Ployz Cloud</CardTitle>
                </div>
                <CardDescription>
                  Good fit if you want the workflow fast: GitHub sign-in,
                  projects, environments, and hosted billing.
                </CardDescription>
              </CardHeader>
              <Separator />
              <CardContent className="flex flex-col gap-2 text-sm text-muted-foreground">
                <p>1. Sign in with GitHub</p>
                <p>2. Create a project and environment</p>
                <p>3. Add a service from GitHub or a container image</p>
                <p>4. Deploy now and add servers later if you need them</p>
              </CardContent>
              <CardFooter>
                <Link to="/auth" className={buttonVariants({ className: 'w-full' })}>
                  Start in cloud
                </Link>
              </CardFooter>
            </Card>

            <Card>
              <CardHeader>
                <div className="flex items-center gap-2">
                  <ServerIcon className="size-4 text-primary" />
                  <CardTitle className="text-base">Self-host the runtime</CardTitle>
                </div>
                <CardDescription>
                  Good fit if you want the runtime on your own infrastructure and
                  care about how workloads move between machines.
                </CardDescription>
              </CardHeader>
              <Separator />
              <CardContent className="flex flex-col gap-2 text-sm text-muted-foreground">
                <p>1. Provision a Linux server</p>
                <p>2. Install the runtime and start a mesh</p>
                <p>3. Enroll more machines with the Cloud bootstrap command</p>
                <p>4. Preview and apply deploys with the CLI</p>
              </CardContent>
              <CardFooter>
                <SiteAction
                  href={runtimeRepoHref}
                  rel="noreferrer"
                  target="_blank"
                  variant="outline"
                  className="w-full"
                >
                  Runtime quickstart
                </SiteAction>
              </CardFooter>
            </Card>
          </div>
        </div>
      </section>

      <section className="border-y bg-muted/20 py-16">
        <div className="mx-auto max-w-5xl px-4">
          <div className="mb-8 text-center">
            <h2 className="text-2xl font-semibold tracking-tight">
              What Ployz actually gives you
            </h2>
            <p className="mt-3 text-muted-foreground">
              The strongest part of the product is the operating model, not just
              the setup automation.
            </p>
          </div>
          <div className="grid gap-6 md:grid-cols-3">
            {[
              {
                icon: GitBranchIcon,
                title: 'One machine to fleet',
                body: 'Start small. Add more machines only when you need the capacity.',
              },
              {
                icon: TerminalIcon,
                title: 'Preview then apply',
                body: 'The runtime supports a real preview and apply workflow instead of vague deploy magic.',
              },
              {
                icon: ServerIcon,
                title: 'Replace weak servers',
                body: 'Move workloads onto better hardware without turning it into a special migration project.',
              },
              {
                icon: CloudIcon,
                title: 'Review before deploy',
                body: 'Stage changes, inspect diffs, then deploy when you are ready instead of shipping blind edits.',
              },
              {
                icon: TerminalIcon,
                title: 'Built-in operational basics',
                body: 'Backups, scheduled jobs, branch deploys, and private service networking belong in the platform, not in extra glue code.',
              },
            ].map((item) => {
              const Icon = item.icon

              return (
                <Card key={item.title}>
                  <CardHeader>
                    <div className="flex items-center gap-2">
                      <Icon className="size-4 text-primary" />
                      <CardTitle className="text-base">{item.title}</CardTitle>
                    </div>
                    <CardDescription>{item.body}</CardDescription>
                  </CardHeader>
                </Card>
              )
            })}
          </div>
        </div>
      </section>

      <section className="border-y bg-muted/20 py-16">
        <div className="mx-auto max-w-3xl px-4">
          <div className="mb-8 flex items-center gap-2">
            <TerminalIcon className="size-4 text-primary" />
            <h2 className="text-xl font-semibold tracking-tight">CLI quick reference</h2>
          </div>
          <CodeBlock
            label="ployzctl CLI"
            code={cliCommands.map((c) => `# ${c.description}\n${c.cmd}`).join('\n\n')}
          />
          <div className="mt-6 grid gap-3 sm:grid-cols-2">
            {cliCommands.map((cmd) => (
              <div key={cmd.cmd} className="rounded-lg border bg-muted/30 p-3">
                <p className="mb-1 text-xs font-mono font-medium">
                  {cmd.cmd.startsWith('npx ')
                    ? cmd.cmd.split(' ').slice(1, 3).join(' ')
                    : cmd.cmd.split(' ').slice(0, 2).join(' ')}
                </p>
                <p className="text-xs text-muted-foreground">{cmd.description}</p>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Links section */}
      <section className="py-20">
        <div className="mx-auto max-w-3xl px-4 text-center">
          <h2 className="mb-4 text-2xl font-semibold tracking-tight">
            More resources
          </h2>
          <p className="mb-8 text-muted-foreground">
            Read the repos directly if you want the raw implementation.
          </p>
          <div className="flex flex-wrap items-center justify-center gap-3">
            <SiteAction
              href={dashboardRepoHref}
              rel="noreferrer"
              target="_blank"
              variant="outline"
            >
              Dashboard repo
            </SiteAction>
            <SiteAction
              href={runtimeRepoHref}
              rel="noreferrer"
              target="_blank"
              variant="outline"
            >
              Runtime repo
            </SiteAction>
          </div>
        </div>
      </section>
    </main>
  )
}
