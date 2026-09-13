import { createFileRoute } from '@tanstack/react-router'
import { CheckIcon } from 'lucide-react'
import {
  runtimeRepoHref,
} from '#/components/marketing/links'
import { buildMarketingMeta } from '#/components/marketing/meta'
import { SiteAction } from '#/components/marketing/SiteAction'
import { Badge } from '#/components/ui/badge'
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from '#/components/ui/tabs'
import { PageHero } from '#/components/marketing/PageHero'
import { CodeBlock } from '#/components/marketing/CodeBlock'
import { CTASection } from '#/components/marketing/CTASection'
import { Link } from '@tanstack/react-router'

export const Route = createFileRoute('/_public/pricing')({
  head: () => ({
    meta: buildMarketingMeta({
      title: 'Pricing - Ployz',
      description:
        'Choose hosted cloud plans or self-host the Ployz runtime for free. Start on one machine and grow when you need to.',
    }),
  }),
  component: PricingPage,
})

const cloudPlans = [
  {
    name: 'Free',
    price: '$0',
    period: 'per month',
    description: 'For experimenting and personal projects',
    features: [
      'Unlimited servers, deployments, apps, and databases',
      '2 environments',
      'GitHub or image deploys',
      'Community support',
      'No log storage, backups, or scheduled jobs',
    ],
    cta: 'Start in cloud',
    ctaVariant: 'outline' as const,
  },
  {
    name: 'Hobby',
    price: '$9',
    period: 'per month',
    description: 'For individual developers who want real operational tooling',
    features: [
      'Everything in Free',
      'Log storage and metrics retention',
      'Deployment diffs with per-field review',
      'Push-to-deploy with optional CI gating',
      'Volume and database backups',
      'Scheduled jobs',
      'More environments and email support',
    ],
    cta: 'Start in cloud',
    ctaVariant: 'default' as const,
    highlight: true,
  },
  {
    name: 'Pro',
    price: '$29',
    period: 'per month',
    description: 'For production workloads',
    features: [
      'Everything in Hobby',
      'Unlimited environments, backups, and scheduled jobs',
      'Longer log and metric retention',
      'Priority support',
    ],
    cta: 'Start in cloud',
    ctaVariant: 'outline' as const,
  },
]

const selfHostedFeatures = [
  'Free and open source runtime',
  'Start on one machine and enroll more with the bootstrap command',
  'Preview and apply deploy workflow',
  'Git or image based services',
  'Same canvas and staged deploy workflow as cloud',
]

function PricingPage() {
  return (
    <main>
      <PageHero
        eyebrow="Pricing"
        title="Pick the control surface you want"
        subtitle="Use the hosted cloud product, or self-host the Ployz runtime for free. The core operating model stays the same."
      />

      <section className="py-20">
        <div className="mx-auto max-w-5xl px-4">
          <Tabs defaultValue="cloud">
            <TabsList className="mx-auto mb-12 flex w-fit">
              <TabsTrigger value="cloud">Cloud</TabsTrigger>
              <TabsTrigger value="self-hosted">Self-hosted</TabsTrigger>
            </TabsList>

            <TabsContent value="cloud">
              <div className="grid gap-6 lg:grid-cols-3">
                {cloudPlans.map((plan) => (
                  <Card
                    key={plan.name}
                    className={plan.highlight ? 'border-primary' : ''}
                  >
                    <CardHeader>
                      <div className="flex items-center justify-between">
                        <CardTitle className="text-base">{plan.name}</CardTitle>
                        {plan.highlight ? (
                          <Badge variant="default" className="text-xs">
                            Best place to start
                          </Badge>
                        ) : null}
                      </div>
                      <div className="flex items-baseline gap-1">
                        <span className="text-3xl font-semibold">{plan.price}</span>
                        <span className="text-sm text-muted-foreground">
                          / {plan.period}
                        </span>
                      </div>
                      <CardDescription>{plan.description}</CardDescription>
                    </CardHeader>
                    <Separator />
                    <CardContent className="flex flex-col gap-2">
                      {plan.features.map((f) => (
                        <div key={f} className="flex items-center gap-2 text-sm">
                          <CheckIcon className="size-3.5 shrink-0 text-success" />
                          <span>{f}</span>
                        </div>
                      ))}
                    </CardContent>
                    <CardFooter>
                      <Link
                        to="/auth"
                        className={buttonVariants({
                          variant: plan.ctaVariant,
                          className: 'w-full',
                        })}
                      >
                        {plan.cta}
                      </Link>
                    </CardFooter>
                  </Card>
                ))}
              </div>
              <p className="mt-6 text-center text-sm text-muted-foreground">
                Cloud sign-in uses GitHub. After that you create a project,
                choose Git or image, and deploy.
              </p>
            </TabsContent>

            <TabsContent value="self-hosted">
              <div className="mx-auto max-w-3xl">
                <Card>
                  <CardHeader>
                    <Badge variant="outline" className="w-fit">
                      Open source runtime
                    </Badge>
                    <CardTitle>Self-host Ployz for free</CardTitle>
                    <CardDescription>
                      Good fit if you want the runtime on your own infrastructure
                      and care more about the operator model than hosted SaaS
                      conveniences.
                    </CardDescription>
                  </CardHeader>
                  <Separator />
                  <CardContent className="flex flex-col gap-6">
                    <div className="grid gap-3 md:grid-cols-2">
                      {selfHostedFeatures.map((feature) => (
                        <div key={feature} className="flex items-start gap-2 text-sm">
                          <CheckIcon className="mt-0.5 size-3.5 shrink-0 text-success" />
                          <span>{feature}</span>
                        </div>
                      ))}
                    </div>
                    <CodeBlock
                      label="Quickstart"
                      code={`# Install the runtime
ployzctl daemon install --runtime docker

# Start a mesh
ployzctl mesh init my-network

# Deploy your first service
ployzctl deploy service whoami \\
  --image traefik/whoami:latest \\
  -p 8080:80`}
                    />
                  </CardContent>
                  <CardFooter className="gap-3">
                    <SiteAction to="/docs" className="flex-1">
                      Self-host docs
                    </SiteAction>
                    <SiteAction
                      href={runtimeRepoHref}
                      rel="noreferrer"
                      target="_blank"
                      variant="outline"
                      className="flex-1"
                    >
                      Runtime repo
                    </SiteAction>
                  </CardFooter>
                </Card>
              </div>
            </TabsContent>
          </Tabs>
        </div>
      </section>

      {/* FAQ */}
      <section className="border-b py-16">
        <div className="mx-auto max-w-2xl px-4">
          <h2 className="mb-8 text-center text-xl font-semibold tracking-tight">
            Common questions
          </h2>
          <div className="flex flex-col gap-6">
            {[
              {
                q: 'Is the Free plan really free forever?',
                a: 'Yes. No credit card required to sign up. The Free plan stays free — we only charge if you upgrade to Pro.',
              },
              {
                q: 'What counts as an "app"?',
                a: 'Each deployed service is one app. A web server and a separate worker running in the same project each count as one app.',
              },
              {
                q: 'Is self-hosting truly unlimited?',
                a: 'Yes. The self-hosted runtime is open source and has no per-app or per-seat limits. You pay for your own servers, not for using Ployz.',
              },
              {
                q: 'Can I switch between Cloud and self-hosted?',
                a: 'Yes. The CLI and the cloud platform use the same concepts. Export your config and import it wherever you want to run.',
              },
              {
                q: 'What happens when my app crashes?',
                a: 'You configure healthchecks and restart policies per service. Ployz monitors your containers and automatically restarts them based on your rules — on failure, always, or never.',
              },
              {
                q: 'Do you support monorepos?',
                a: 'Yes. Each service can point at a different subdirectory in the same repo. One repository, as many services as you need.',
              },
              {
                q: 'How do my services talk to each other?',
                a: 'Every service gets a private DNS name automatically. Your backend can reach your database by name — no hardcoded IPs or service discovery configuration.',
              },
            ].map((item) => (
              <div key={item.q} className="flex flex-col gap-1">
                <p className="font-medium">{item.q}</p>
                <p className="text-sm text-muted-foreground">{item.a}</p>
              </div>
            ))}
          </div>
        </div>
      </section>

      <CTASection
        title="Ready to ship?"
        subtitle="Start for free on the cloud, or self-host on your own server — no credit card required."
        primaryLabel="Start for free"
        secondaryLabel="Self-host free"
        secondaryHref="/docs"
      />
    </main>
  )
}
