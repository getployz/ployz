import {
  ChevronsUpDownIcon,
  CircleCheckIcon,
  GitPullRequestIcon,
  HistoryIcon,
  LayoutGridIcon,
  PackageIcon,
  PlusIcon,
  SearchIcon,
  ServerIcon,
  SlidersHorizontalIcon,
  TerminalIcon,
  WorkflowIcon,
} from "lucide-react";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Badge } from "#/components/ui/badge";
import { buttonVariants } from "#/components/ui/button-variants";
import { Kbd } from "#/components/ui/kbd";
import { cn } from "#/lib/utils";
import { BrowserWindow, Line, ServerFace, ServiceCard, StatusDot, Terminal, Wires, revealDelay, useInView, useLoop } from "#/components/marketing/kit";
import type { Tone, Wire } from "#/components/marketing/kit";

// The lander's pictures. Each is the dashboard, or the server under it, doing one thing in the
// product's own words and colours. Scenes are decoration: inert inside, and named for screen readers.
// Addresses sit under up.ployz.app, where Hosted DNS grants Cluster Domains.

const gitIcon = <GitHubMarkIcon />;
const imageIcon = <PackageIcon />;

// ---- the hero: the dashboard running an app, and the server it runs on --------------------------------

const HERO_WIRES: readonly Wire[] = [
  { from: "web", to: "ticker", arrow: false, packets: 3, seconds: 1.6 },
  { from: "api", to: "web", packets: 3, seconds: 1.8 },
  { from: "postgres", to: "api", packets: 3, seconds: 1.8 },
  { from: "redis", to: "api", packets: 2, seconds: 2.4 },
  { from: "redis", to: "worker", packets: 2, seconds: 2.8 },
];

export function HeroScene() {
  return (
    <div role="img" aria-label="The Ployz dashboard running web, api and worker over postgres and redis while requests flow through them, on your own server" className="w-full">
      <div inert className="overflow-hidden rounded-2xl border border-border bg-background">
        <div className="flex">
          <Rail />
          <div className="min-w-0 flex-1">
            <TopBar />
            <div className="lp-dots relative isolate flex flex-col items-center gap-12 px-4 pt-6 pb-10 sm:px-10">
              <Ticker id="ticker" host="web.acme.up.ployz.app" />
              <ServiceCard id="web" icon={gitIcon} name="web" sub="acme/web" status="Deployed · 2 containers observed" />
              <div className="grid w-full grid-cols-1 justify-items-center gap-12 md:grid-cols-2 md:gap-x-8">
                <ServiceCard id="api" icon={gitIcon} name="api" sub="acme/api" status="Deployed · 2 containers observed" />
                <ServiceCard id="worker" icon={gitIcon} name="worker" sub="acme/worker" status="Deployed" className="hidden md:flex" />
                <ServiceCard id="postgres" icon={imageIcon} name="postgres" sub="postgres:17" status="Deployed" />
                <ServiceCard id="redis" icon={imageIcon} name="redis" sub="redis:7" status="Deployed" className="hidden md:flex" />
              </div>
              <Wires wires={HERO_WIRES} />
            </div>
          </div>
        </div>
      </div>
      <div aria-hidden className="flex justify-around px-[18%]">
        <i className="lp-leg" />
        <i className="lp-leg" />
      </div>
      <ServerFace name="server-1" spec="8 vCPU · 32 GB" ip="203.0.113.12" />
    </div>
  );
}

// The rail's places (dashboard-navigation-model): an Environment's four, then the organization's two.
const RAIL_PLACES = [WorkflowIcon, HistoryIcon, TerminalIcon, SlidersHorizontalIcon];
const RAIL_ORG = [LayoutGridIcon, ServerIcon];

function Rail() {
  const place = (Icon: typeof ServerIcon, key: number, current = false) => (
    <span key={key} className={cn("flex size-9 items-center justify-center rounded-lg text-muted-foreground", current && "bg-muted text-foreground")}>
      <Icon className="size-4" />
    </span>
  );
  return (
    <div className="hidden w-14 shrink-0 flex-col items-center gap-1 border-r border-border py-3 md:flex">
      {RAIL_PLACES.map((Icon, i) => place(Icon, i, i === 0))}
      <span className="my-2 h-px w-6 bg-border" />
      {RAIL_ORG.map((Icon, i) => place(Icon, i))}
    </div>
  );
}

function TopBar() {
  return (
    <div className="flex h-12 items-center gap-1 border-b border-border px-3">
      <span className={buttonVariants({ variant: "ghost", size: "sm" })}>
        acme
        <ChevronsUpDownIcon />
      </span>
      <span className="text-muted-foreground">/</span>
      <span className={buttonVariants({ variant: "outline", size: "sm" })}>
        production
        <ChevronsUpDownIcon />
      </span>
      <span className={cn(buttonVariants({ variant: "outline", size: "sm" }), "ml-auto hidden sm:inline-flex")}>
        <SearchIcon />
        Find
        <Kbd>/</Kbd>
      </span>
      <span className={cn(buttonVariants({ variant: "ink", size: "sm" }), "ml-auto sm:ml-0")}>
        <PlusIcon />
        Create
      </span>
    </div>
  );
}

/** Requests answered, scrolling by: proof the app is serving, without claiming any metrics. */
function Ticker({ id, host }: { id: string; host: string }) {
  return (
    <div data-node={id} className="lp-ticker w-full max-w-sm">
      <StatusDot tone="success" />
      <span className="shrink-0">GET {host}</span>
      <div className="lp-ticker-window">
        <div className="lp-ticker-row">
          {Array.from({ length: 24 }, (_, i) => (
            <i key={i}>200</i>
          ))}
        </div>
      </div>
    </div>
  );
}

// ---- one command: a bare root prompt, handed to Ployz (#1225's `ployz up`) -----------------------------

const START_WIRES: readonly Wire[] = [
  { from: "bare", to: "up", across: true, packets: 2, seconds: 2, className: "max-lg:hidden" },
  { from: "up", to: "live", across: true, packets: 2, seconds: 2, className: "max-lg:hidden" },
];

export function GetStartedScene() {
  const [ref, inview] = useInView<HTMLDivElement>();
  return (
    <div ref={ref} data-inview={inview} role="img" aria-label="A bare server's root prompt, then ployz login and ployz up turn it into a live https address" className="relative isolate grid grid-cols-1 items-center gap-6 lg:grid-cols-[minmax(0,15rem)_minmax(0,1.3fr)_1fr] lg:gap-12">
      <div data-node="bare">
        <Terminal title="ssh root@203.0.113.12">
          <Line prompt="root@server:~#">
            <span className="lp-caret" />
          </Line>
        </Terminal>
      </div>
      <div data-node="up">
        <Terminal light title="~/acme">
          <Line order={0} prompt="$">
            ployz login
          </Line>
          <Line order={1} prompt="$">
            ployz up --server root@203.0.113.12
          </Line>
          <Line order={2} className="lp-dim">
            Installing Ployz on 203.0.113.12
          </Line>
          <Line order={3} className="lp-dim">
            Building web
          </Line>
          <Line order={4}>
            <span className="lp-ok">✓</span> https://web.acme.up.ployz.app
          </Line>
        </Terminal>
      </div>
      <div data-node="live" className="lp-late" style={inview ? { transitionDelay: revealDelay(5) } : undefined} inert>
        <BrowserWindow url="web.acme.up.ployz.app">
          <SiteMock />
        </BrowserWindow>
      </div>
      <Wires wires={START_WIRES} />
    </div>
  );
}

// ---- what it runs: Railpack 0.39.0's providers (core/crates/ployz-build), plus any image ------------------

// landing.css's lp-words keyframes step through exactly eight words: change both together.
const DEPLOY_WORDS = ["Next.js", "Rails", "Laravel", "Django", "Go", "Bun", "Postgres", "anything"];

/** "Deploy" and the stack scrolling past it. The first word repeats at the end, so the loop is seamless. */
export function DeployTicker() {
  return (
    <div role="img" aria-label={`Deploy ${DEPLOY_WORDS.join(", ")}`} className="lp-deploy">
      <span className="lp-deploy-label">Deploy</span>
      <span className="lp-deploy-window">
        <span className="lp-deploy-words">
          {[...DEPLOY_WORDS, DEPLOY_WORDS[0]].map((word, i) => (
            <span key={`${word}-${i}`}>{word}</span>
          ))}
        </span>
      </span>
    </div>
  );
}

// ---- a preview environment for every pull request ------------------------------------------------------

const PREVIEW_WIRES: readonly Wire[] = [{ from: "preview", to: "pull-request", arrow: false, packets: 3, seconds: 1.8 }];

export function PreviewScene() {
  return (
    <div role="img" aria-label="Pull request #142 gets its own preview environment at its own https address, showing the new pricing section" inert className="relative isolate flex flex-col items-center gap-10">
      <PullRequestCard id="pull-request" />
      <div data-node="preview" className="w-full">
        <BrowserWindow url="web-pr-142.acme.up.ployz.app">
          <SiteMock pricing />
        </BrowserWindow>
      </div>
      <Wires wires={PREVIEW_WIRES} />
    </div>
  );
}

function PullRequestCard({ id }: { id: string }) {
  return (
    <div data-node={id} className="w-full max-w-sm rounded-xl border border-border bg-background p-4 text-sm">
      <div className="flex items-center gap-2">
        <GitPullRequestIcon className="size-4 shrink-0 text-success" />
        <span className="truncate font-medium">Add pricing page</span>
        <span className="text-muted-foreground">#142</span>
      </div>
      <p className="mt-1 truncate font-mono text-xs text-muted-foreground">feature/pricing → main</p>
      <div className="mt-3 flex items-center gap-2 border-t border-border pt-3 text-xs">
        <CircleCheckIcon className="size-3.5 shrink-0 text-success" />
        <span className="shrink-0 font-medium">pr-142 deployed</span>
        <span className="ml-auto truncate font-mono text-muted-foreground">web-pr-142.acme.up.ployz.app</span>
      </div>
    </div>
  );
}

const PLANS = [
  { name: "Starter", price: "$0" },
  { name: "Pro", price: "$12" },
  { name: "Team", price: "$39" },
];

/** The customer's own site: their brand, not ours, so neutral. `pricing` adds the pull request's new section. */
function SiteMock({ pricing }: { pricing?: boolean }) {
  return (
    <div className="flex flex-col gap-6 p-5 sm:p-7">
      <div className="flex items-center gap-4 text-xs text-muted-foreground">
        <span className="text-sm font-bold tracking-tight text-foreground">acme</span>
        <span className="ml-auto">Product</span>
        <span>Pricing</span>
        <span>Log in</span>
      </div>
      <div className="flex flex-col gap-2 py-2">
        <p className="text-2xl font-semibold tracking-tight text-balance">Invoices that chase themselves.</p>
        <p className="text-sm text-muted-foreground">Send, remind and get paid, without the follow-up emails.</p>
        <span className={cn(buttonVariants({ variant: "ink", size: "sm" }), "mt-2 w-fit")}>Start free trial</span>
      </div>
      {pricing ? (
        <div className="relative rounded-xl border-2 border-dashed border-success-border p-3 pt-5">
          <Badge variant="success" className="absolute -top-2.5 left-3">
            New
          </Badge>
          <div className="grid grid-cols-3 gap-2.5">
            {PLANS.map(({ name, price }) => (
              <div key={name} className="flex flex-col gap-1 rounded-lg border border-border p-3">
                <span className="text-xs text-muted-foreground">{name}</span>
                <span className="text-lg font-semibold">
                  {price}
                  <span className="text-xs font-normal text-muted-foreground">/mo</span>
                </span>
              </div>
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}

// ---- review: an edit waits until Deploy -------------------------------------------------------

// One service's card through a deploy, as ServiceNode shows it (getServiceDeploymentSemantics).
type CardLook = { tone: Tone; state?: "info"; status: string };
type ReviewFrame = { ms: number; card: CardLook; bar: "staged" | "deploying" | null };
// Staged is where it rests: the changed card in blue, the pink Apply bar and Deploy, in one still.
const REVIEW_STAGED: ReviewFrame = { ms: 3000, card: { tone: "quiet", state: "info", status: "1 change" }, bar: "staged" };
const REVIEW_FRAMES: readonly [ReviewFrame, ...ReviewFrame[]] = [
  { ms: 2200, card: { tone: "quiet", status: "Deployed · 1 container observed" }, bar: null },
  REVIEW_STAGED,
  { ms: 1800, card: { tone: "quiet", status: "Deploying…" }, bar: "deploying" },
  { ms: 3000, card: { tone: "quiet", status: "Deployed · 3 containers observed" }, bar: null },
];

export function ReviewScene() {
  const [ref, inview] = useInView<HTMLDivElement>();
  const frame = useLoop(REVIEW_FRAMES, inview === true, REVIEW_STAGED);
  return (
    <div ref={ref} role="img" aria-label="A change to web waits in pink until Deploy ships it" inert className="lp-dots relative flex min-h-80 w-full flex-col items-center justify-center rounded-xl border border-border px-4 pt-8 pb-28">
      <ServiceCard icon={gitIcon} name="web" sub="acme/web" {...frame.card} />
      <BottomBarPicture bar={frame.bar} />
    </div>
  );
}

/** The canvas's bottom bar, drawn with its own classes (styles.css): changes to deploy, then the running attempt. */
function BottomBarPicture({ bar }: { bar: ReviewFrame["bar"] }) {
  if (bar === "staged") {
    return (
      <div className="bottom-bar">
        <div className="bottom-bar-row" data-staged>
          <div className="min-w-0 flex-1 pr-3">
            <p className="truncate text-sm font-medium text-changed-deep tabular-nums">Apply 1 change</p>
          </div>
          <span className={buttonVariants({ variant: "outline" })}>Details</span>
          <span className={buttonVariants({ variant: "intent" })}>Deploy</span>
        </div>
      </div>
    );
  }
  if (bar === "deploying") {
    return (
      <div className="bottom-bar">
        <div className="bottom-bar-row">
          <span className="flex">
            <DeploymentStatusIcon status="deploying" />
          </span>
          <div className="min-w-0 flex-1 pr-3">
            <p className="truncate text-sm font-medium">Deploying · Scale web to 3</p>
            <p className="truncate text-xs text-muted-foreground">web · Starting containers</p>
          </div>
          <span className={buttonVariants({ variant: "outline" })}>Logs</span>
        </div>
      </div>
    );
  }
  return null;
}

// ---- the bill: Ployz's part is flat; the server is your provider's ---------------------------------------

const RECEIPT = [
  { item: "Ployz", detail: "Preview environments, servers, replicas", price: "$0" },
  { item: "Custom domains", detail: "Optional", price: "$9" },
];

export function ReceiptScene() {
  return (
    <div role="img" aria-label="Ployz's monthly bill: $0 for Ployz, and $9 for custom domains if you want them; your provider bills you for the server" className="rounded-xl border border-border bg-background p-6 font-mono text-sm">
      <div className="flex justify-between text-muted-foreground">
        <span>Your Ployz bill</span>
        <span>/mo</span>
      </div>
      <div className="mt-4 flex flex-col divide-y divide-dashed divide-border border-y border-dashed border-border">
        {RECEIPT.map(({ item, detail, price }) => (
          <div key={item} className="flex items-baseline gap-4 py-4">
            <div className="flex min-w-0 flex-col gap-0.5">
              <span>{item}</span>
              <span className="text-xs text-muted-foreground">{detail}</span>
            </div>
            <span className="ml-auto shrink-0 text-lg">{price}</span>
          </div>
        ))}
      </div>
      <p className="mt-4 text-xs text-muted-foreground">Your servers are billed by your provider.</p>
    </div>
  );
}

// ---- the last gag: the old root prompt, done with ------------------------------------------------------

export function ExitPrompt() {
  return (
    <Terminal title="ssh root@203.0.113.12" className="w-full max-w-xs text-left">
      <Line prompt="root@server:~#">exit</Line>
    </Terminal>
  );
}
