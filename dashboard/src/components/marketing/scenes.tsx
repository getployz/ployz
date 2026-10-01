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
import { useId } from "react";
import type { ReactNode } from "react";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { DATABASE_LOGOS } from "#/components/icons/database-logos";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Badge } from "#/components/ui/badge";
import { buttonVariants } from "#/components/ui/button-variants";
import { Kbd } from "#/components/ui/kbd";
import { cn } from "#/lib/utils";
import { BrowserWindow, Line, Packet, ServerFace, ServiceCard, StatusDot, Terminal, Wires, revealDelay, useInView, useLoop, usePinProgress } from "#/components/marketing/kit";
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

// ---- no load balancer to rent: scrolling splits it into every server -------------------------------------
// What core does: each server runs the proxy (Caddy) and picks a healthy copy on any server over WireGuard.

/** The picture's layout in viewBox units: a wide one, and a narrow one for phones. */
type BalancerLayout = {
  box: [number, number];
  visitors: [number, number];
  bar: { x: number; y: number; w: number; h: number };
  sticker: [number, number];
  /** Each server's centre x; `server` is their shared centre y and size. */
  servers: [number, number, number];
  server: { y: number; w: number; h: number };
  badge: [number, number];
  /**
   * The private network: a pipe between each pair of servers, as [where it plugs in, measured from the
   * server's centre, how deep it dips]. Neighbours plug in on their facing sides; the pipe between the ends
   * plugs in on the outer sides and wraps underneath.
   */
  pipes: { width: number; near: [number, number]; far: [number, number] };
  network: number;
  /** Whether each app's chip names it beside its logo; phones show the logos alone. */
  labels: boolean;
};

const WIDE: BalancerLayout = {
  box: [1000, 530],
  visitors: [500, 44],
  bar: { x: 150, y: 142, w: 700, h: 56 },
  sticker: [805, 142],
  servers: [230, 500, 770],
  server: { y: 350, w: 200, h: 84 },
  badge: [110, 22],
  pipes: { width: 8, near: [56, 60], far: [44, 132] },
  network: 520,
  labels: true,
};

const NARROW: BalancerLayout = {
  box: [400, 448],
  visitors: [200, 30],
  bar: { x: 16, y: 104, w: 368, h: 52 },
  sticker: [330, 102],
  servers: [72, 200, 328],
  server: { y: 318, w: 116, h: 76 },
  badge: [104, 20],
  pipes: { width: 5, near: [38, 34], far: [28, 76] },
  network: 438,
  labels: false,
};

// What each server runs: a copy of web on every one, and Postgres on the last.
const APPS = { web: { Logo: GitHubMarkIcon, width: 54 }, postgres: { Logo: DATABASE_LOGOS.postgres, width: 84 } };
type App = keyof typeof APPS;
const SERVER_APPS = [["web"], ["web"], ["web", "postgres"]] as const;

/** Each app's chip along a server's bottom row, from its `left` edge. */
function chipRow(apps: readonly App[], left: number, labels: boolean) {
  const width = (app: App) => (labels ? APPS[app].width : 22);
  return apps.map((app, n) => ({ app, w: width(app), x: left + 12 + apps.slice(0, n).reduce((sum, a) => sum + width(a) + 6, 0) }));
}

/**
 * Pinned under the header for half a screen of scrolling, which splits the load balancer into three: one
 * inside each server. The split takes the first 80% of it, so the result holds a moment before the page
 * moves on.
 */
export function BalancerScene({ heading, caption }: { heading: ReactNode; caption: ReactNode }) {
  const [ref, progress] = usePinProgress<HTMLElement>();
  const t = Math.min(1, progress / 0.8);
  return (
    // A margin, not padding, keeps the frame flush with the section top, which usePinProgress measures from.
    <section ref={ref} className="lp-pin mt-16 px-5 md:mt-24">
      <div className="lp-pin-frame mx-auto flex max-w-6xl flex-col justify-center gap-6 py-6 md:gap-8">
        {heading}
        <div
          role="img"
          aria-label="Visitors reach three servers, one also running Postgres, through one load balancer you rent; scrolling splits it into three, one inside each server, and the servers link up over a private network"
          className="flex max-h-136 min-h-0 flex-1 rounded-3xl bg-(--color-paper) p-3 max-sm:max-h-96 sm:p-6"
        >
          <BalancerPicture layout={WIDE} t={t} className="size-full max-sm:hidden" />
          <BalancerPicture layout={NARROW} t={t} className="size-full sm:hidden" />
        </div>
        {caption}
      </div>
    </section>
  );
}

const lerp = (a: number, b: number, x: number) => a + (b - a) * x;
const easeInOut = (x: number) => (x < 0.5 ? 4 * x ** 3 : 1 - (-2 * x + 2) ** 3 / 2);

/** The picture at `t`: 0 is the usual way, 1 is with Ployz. Requests only flow at either end. */
function BalancerPicture({ layout: l, t, className }: { layout: BalancerLayout; t: number; className: string }) {
  const phase = (from: number, to: number) => Math.min(1, Math.max(0, (t - from) / (to - from)));
  const [vx, vy] = l.visitors;
  const out = vy + 19;
  const top = l.server.y - l.server.h / 2;
  const bottom = l.server.y + l.server.h / 2;
  const [bw, bh] = l.badge;
  const third = l.bar.w / 3;
  const [sx, sy] = l.sticker;
  const [s1, s2, s3] = l.servers;
  const w = l.pipes.width;
  const [nearPlug, nearDepth] = l.pipes.near;
  const [farPlug, farDepth] = l.pipes.far;
  // A U from one server's bottom edge down and across to another's.
  const pipe = (x1: number, x2: number, depth: number) => `M${x1} ${bottom}C${x1} ${bottom + depth} ${x2} ${bottom + depth} ${x2} ${bottom}`;
  const pipes = [pipe(s1 + nearPlug, s2 - nearPlug, nearDepth), pipe(s2 + nearPlug, s3 - nearPlug, nearDepth), pipe(s1 - farPlug, s3 + farPlug, farDepth)];
  // Both layouts are on the page at once, so each needs its own ids.
  const id = useId();
  const shade = `${id}-shade`;
  const glow = `${id}-glow`;
  const stroke = `url(#${shade})`;
  return (
    <svg viewBox={`0 0 ${l.box[0]} ${l.box[1]}`} className={className}>
      <defs>
        <linearGradient id={shade}>
          {[1, 2, 3, 4, 5].map((n) => (
            <stop key={n} offset={(n - 1) / 4} style={{ stopColor: `var(--lp-pipe-${n})` }} />
          ))}
        </linearGradient>
        <filter id={glow} x="-20%" y="-60%" width="140%" height="220%">
          <feGaussianBlur stdDeviation={w * 0.8} />
        </filter>
      </defs>
      <g style={{ opacity: 1 - phase(0, 0.3) }}>
        <path d={`M${vx} ${out}V${l.bar.y}`} className="lp-lb-line" />
        {l.servers.map((x) => (
          <path key={x} d={`M${x} ${l.bar.y + l.bar.h}V${top}`} className="lp-lb-line" />
        ))}
      </g>
      <g style={{ opacity: phase(0.7, 0.85) }}>
        {l.servers.map((x) => (
          <path key={x} d={`M${vx} ${out}L${x} ${top - bh / 2}`} className="lp-lb-dns" />
        ))}
        {pipes.map((d, i) => {
          // Each pipe lays itself in just after the one before: a pearly tube over its own glow, with a glint
          // sweeping through once it's in.
          const unlaid = 1 - phase(0.74 + i * 0.04, 0.92 + i * 0.03);
          return (
            <g key={d}>
              <path d={d} pathLength={1} strokeWidth={w * 2} filter={`url(#${glow})`} className="lp-lb-pipe lp-lb-glow" style={{ stroke, strokeDashoffset: unlaid }} />
              <path d={d} pathLength={1} strokeWidth={w} className="lp-lb-pipe" style={{ stroke, strokeDashoffset: unlaid }} />
              <path d={d} pathLength={1} strokeWidth={w * 0.35} className="lp-lb-sheen" style={{ opacity: phase(0.94, 1), animationDelay: `${-i * 0.8}s` }} />
            </g>
          );
        })}
        <text x={l.servers[1]} y={l.network} textAnchor="middle" className="lp-lb-note">
          private network
        </text>
      </g>
      {t === 0
        ? l.servers.map((x, i) =>
            [0, 1].map((n) => (
              <Packet key={`${x}-${n}`} path={`M${vx} ${out}V${l.bar.y + l.bar.h / 2}H${x}V${l.server.y}`} seconds={2.4} delay={(i + 3 * n) * 0.4} reverse={false} />
            )),
          )
        : null}
      {t === 1
        ? l.servers.map((x, i) =>
            [0, 1].map((n) => <Packet key={`${x}-${n}`} path={`M${vx} ${out}L${x} ${l.server.y}`} seconds={2} delay={i * 0.35 + n} reverse={false} />),
          )
        : null}
      <g className="lp-lb-visitors">
        <rect x={vx - 70} y={vy - 19} width={140} height={38} rx={19} />
        {[0, 1, 2].map((i) => (
          <circle key={i} cx={vx - 46 + i * 11} cy={vy} r={5} />
        ))}
        <text x={vx - 12} y={vy + 4.5}>
          visitors
        </text>
      </g>
      <rect x={l.bar.x} y={l.bar.y} width={l.bar.w} height={l.bar.h} rx={12} className="lp-lb-piece" style={{ opacity: t === 0 ? 1 : 0 }} />
      {l.servers.map((x, i) => {
        const left = x - l.server.w / 2;
        return (
          <g key={x}>
            <rect x={left} y={top} width={l.server.w} height={l.server.h} rx={12} className="lp-lb-card" />
            <text x={left + 14} y={top + 24} className="lp-lb-name">
              server-{i + 1}
            </text>
            {chipRow(SERVER_APPS[i] ?? [], left, l.labels).map(({ app, x: chipX, w: chipW }) => {
              const { Logo } = APPS[app];
              return (
                <g key={app}>
                  <rect x={chipX} y={bottom - 32} width={chipW} height={22} rx={6} className="lp-lb-chip" />
                  <Logo x={chipX + 3.5} y={bottom - 28.5} width={15} height={15} className="lp-lb-logo" />
                  {l.labels ? (
                    <text x={chipX + 24} y={bottom - 17} className="lp-lb-chip-text">
                      {app}
                    </text>
                  ) : null}
                </g>
              );
            })}
          </g>
        );
      })}
      {/* Each third of the bar drops into a server as its badge, carrying the name down with it. */}
      {l.servers.map((x, i) => {
        const p = easeInOut(phase(0.08 + i * 0.06, 0.62 + i * 0.06));
        const px = lerp(l.bar.x + i * third, x - bw / 2, p);
        const py = lerp(l.bar.y, top - bh / 2, p);
        const pw = lerp(third, bw, p);
        const ph = lerp(l.bar.h, bh, p);
        return (
          <g key={x} style={{ opacity: t === 0 ? 0 : 1 }}>
            <rect x={px} y={py} width={pw} height={ph} rx={lerp(12, bh / 2, p)} className="lp-lb-piece" />
            <g transform={`translate(${px + pw / 2} ${py + ph / 2})`} style={{ opacity: phase(0.04, 0.14) }} className="lp-lb-badge">
              <circle cx={-40} r={3} />
              <text x={-32} y={3.8}>
                load balancer
              </text>
            </g>
          </g>
        );
      })}
      <g style={{ opacity: 1 - phase(0, 0.12) }}>
        <text x={l.bar.x + l.bar.w / 2} y={l.bar.y + l.bar.h / 2 + 5} textAnchor="middle" className="lp-lb-bar-title">
          Load balancer
        </text>
      </g>
      <g transform={`translate(0 ${phase(0, 0.35) * 90})`} style={{ opacity: 1 - phase(0, 0.25) }}>
        <g transform={`rotate(-7 ${sx} ${sy})`} className="lp-lb-sticker">
          <rect x={sx - 54} y={sy - 12} width={108} height={24} rx={5} />
          <text x={sx} y={sy + 4} textAnchor="middle">
            $ every month
          </text>
        </g>
      </g>
    </svg>
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
