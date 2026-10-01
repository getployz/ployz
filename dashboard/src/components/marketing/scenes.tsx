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
import type { ReactNode, SVGProps } from "react";
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
  /**
   * Each server's centre x; `server` is their shared centre y and size, drawn as the hero's ServerFace.
   * `detail` adds its spec line and activity lights, where there's room for them.
   */
  servers: [number, number, number];
  server: { y: number; w: number; h: number; detail: boolean };
  badge: [number, number];
  /**
   * The private network: a pipe between each pair of servers, as [where it plugs in, measured from the
   * server's centre, how deep it dips]. Neighbours plug in on their facing sides; the pipe between the ends
   * plugs in on the outer sides and wraps underneath.
   */
  pipes: { width: number; near: [number, number]; far: [number, number] };
  network: number;
};

const WIDE: BalancerLayout = {
  box: [1000, 520],
  visitors: [500, 40],
  bar: { x: 70, y: 120, w: 860, h: 52 },
  sticker: [880, 118],
  servers: [210, 500, 790],
  server: { y: 326, w: 270, h: 116, detail: true },
  badge: [120, 24],
  pipes: { width: 5, near: [70, 56], far: [60, 120] },
  network: 506,
};

const NARROW: BalancerLayout = {
  box: [400, 426],
  visitors: [200, 28],
  bar: { x: 10, y: 92, w: 380, h: 46 },
  sticker: [338, 90],
  servers: [70, 200, 330],
  server: { y: 276, w: 122, h: 104, detail: false },
  badge: [100, 20],
  pipes: { width: 3.5, near: [40, 34], far: [30, 76] },
  network: 414,
};

// Next.js and Laravel's marks from Simple Icons 16.33 (CC0), drawn as database-logos.tsx draws Postgres.
function NextjsLogo(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" {...props}>
      <path d="M18.665 21.978C16.758 23.255 14.465 24 12 24 5.377 24 0 18.623 0 12S5.377 0 12 0s12 5.377 12 12c0 3.583-1.574 6.801-4.067 9.001L9.219 7.2H7.2v9.596h1.615V9.251l9.85 12.727Zm-3.332-8.533 1.6 2.061V7.2h-1.6v6.245Z" />
    </svg>
  );
}

function LaravelLogo(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 24 24" fill="#FF2D20" aria-hidden="true" {...props}>
      <path d="M23.642 5.43a.364.364 0 01.014.1v5.149c0 .135-.073.26-.189.326l-4.323 2.49v4.934a.378.378 0 01-.188.326L9.93 23.949a.316.316 0 01-.066.027c-.008.002-.016.008-.024.01a.348.348 0 01-.192 0c-.011-.002-.02-.008-.03-.012-.02-.008-.042-.014-.062-.025L.533 18.755a.376.376 0 01-.189-.326V2.974c0-.033.005-.066.014-.098.003-.012.01-.02.014-.032a.369.369 0 01.023-.058c.004-.013.015-.022.023-.033l.033-.045c.012-.01.025-.018.037-.027.014-.012.027-.024.041-.034H.53L5.043.05a.375.375 0 01.375 0L9.93 2.647h.002c.015.01.027.021.04.033l.038.027c.013.014.02.03.033.045.008.011.02.021.025.033.01.02.017.038.024.058.003.011.01.021.013.032.01.031.014.064.014.098v9.652l3.76-2.164V5.527c0-.033.004-.066.013-.098.003-.01.01-.02.013-.032a.487.487 0 01.024-.059c.007-.012.018-.02.025-.033.012-.015.021-.03.033-.043.012-.012.025-.02.037-.028.014-.01.026-.023.041-.032h.001l4.513-2.598a.375.375 0 01.375 0l4.513 2.598c.016.01.027.021.042.031.012.01.025.018.036.028.013.014.022.03.034.044.008.012.019.021.024.033.011.02.018.04.024.06.006.01.012.021.015.032zm-.74 5.032V6.179l-1.578.908-2.182 1.256v4.283zm-4.51 7.75v-4.287l-2.147 1.225-6.126 3.498v4.325zM1.093 3.624v14.588l8.273 4.761v-4.325l-4.322-2.445-.002-.003H5.04c-.014-.01-.025-.021-.04-.031-.011-.01-.024-.018-.035-.027l-.001-.002c-.013-.012-.021-.025-.031-.04-.01-.011-.021-.022-.028-.036h-.002c-.008-.014-.013-.031-.02-.047-.006-.016-.014-.027-.018-.043a.49.49 0 01-.008-.057c-.002-.014-.006-.027-.006-.041V5.789l-2.18-1.257zM5.23.81L1.47 2.974l3.76 2.164 3.758-2.164zm1.956 13.505l2.182-1.256V3.624l-1.58.91-2.182 1.255v9.435zm11.581-10.95l-3.76 2.163 3.76 2.163 3.759-2.164zm-.376 4.978L16.21 7.087 14.63 6.18v4.283l2.182 1.256 1.58.908zm-8.65 9.654l5.514-3.148 2.756-1.572-3.757-2.163-4.323 2.489-3.941 2.27z" />
    </svg>
  );
}

// What each server runs: a copy of the app (Next.js and Laravel) on every one, and Postgres on the last.
const APPS = { nextjs: NextjsLogo, laravel: LaravelLogo, postgres: DATABASE_LOGOS.postgres };
const SERVER_APPS = [["nextjs", "laravel"], ["nextjs", "laravel"], ["nextjs", "laravel", "postgres"]] as const;
const SERVER_SPECS = ["8 vCPU · 32 GB", "4 vCPU · 16 GB", "16 vCPU · 64 GB"];

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
          aria-label="Visitors reach three servers running Next.js and Laravel, one also running Postgres, through one load balancer you rent; scrolling splits it into three, one inside each server, and the servers link up over a private network"
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
  const stroke = `url(#${shade})`;
  return (
    <svg viewBox={`0 0 ${l.box[0]} ${l.box[1]}`} className={className}>
      <defs>
        <linearGradient id={shade}>
          {[1, 2, 3, 4, 5].map((n) => (
            <stop key={n} offset={(n - 1) / 4} style={{ stopColor: `var(--lp-pipe-${n})` }} />
          ))}
        </linearGradient>
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
          // Each pipe lays itself in just after the one before, with a glint sweeping through once it's in.
          const unlaid = 1 - phase(0.74 + i * 0.04, 0.92 + i * 0.03);
          return (
            <g key={d}>
              <path d={d} pathLength={1} strokeWidth={w} className="lp-lb-pipe" style={{ stroke, strokeDashoffset: unlaid }} />
              <path d={d} pathLength={1} strokeWidth={w * 0.4} className="lp-lb-sheen" style={{ opacity: phase(0.94, 1), animationDelay: `${-i * 0.8}s` }} />
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
      {/* Each server as the hero draws one (ServerFace): graphite, rack screws, its lights, and the apps it runs. */}
      {l.servers.map((x, i) => {
        const left = x - l.server.w / 2;
        const right = x + l.server.w / 2;
        const { detail } = l.server;
        const inset = detail ? 30 : 18;
        const tile = detail ? 26 : 22;
        return (
          <g key={x}>
            <rect x={left} y={top} width={l.server.w} height={l.server.h} rx={12} className="lp-lb-face" />
            {[top + 18, bottom - 18].flatMap((y) =>
              [left + 11, right - 11].map((screwX) => <circle key={`${screwX}-${y}`} cx={screwX} cy={y} r={3.5} className="lp-lb-screw" />),
            )}
            <text x={left + inset} y={top + (detail ? 36 : 30)} className="lp-lb-name">
              server-{i + 1}
            </text>
            {detail ? (
              <text x={left + inset} y={top + 56} className="lp-lb-spec">
                {SERVER_SPECS[i]}
              </text>
            ) : null}
            {[0, 1, 2].map((k) => (
              <circle key={k} cx={right - (detail ? 60 : 36) + k * (detail ? 13 : 9)} cy={top + (detail ? 31 : 25)} r={detail ? 4 : 2.8} className={`lp-lb-led lp-lb-led-${k}`} />
            ))}
            {detail
              ? Array.from({ length: 8 }, (_, k) => (
                  <rect
                    key={k}
                    x={right - 74 + k * 8}
                    y={top + 48}
                    width={5}
                    height={12}
                    rx={1.5}
                    className="lp-lb-act"
                    style={{ animationDuration: `${[1.3, 1.7, 0.9][k % 3]}s`, animationDelay: `${(-k * 0.29 - i * 0.4).toFixed(2)}s` }}
                  />
                ))
              : null}
            {(SERVER_APPS[i] ?? []).map((app, n) => {
              const Logo = APPS[app];
              const tileX = left + inset + n * (tile + 6);
              const logo = tile - 10;
              return (
                <g key={app}>
                  <rect x={tileX} y={bottom - tile - 12} width={tile} height={tile} rx={6} className="lp-lb-chip" />
                  <Logo x={tileX + 5} y={bottom - tile - 7} width={logo} height={logo} className="lp-lb-logo" />
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
