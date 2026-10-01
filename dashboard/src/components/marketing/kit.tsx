import { useEffect, useRef, useState } from "react";
import type { ReactNode, RefObject } from "react";
import { Position, getBezierPath, getSmoothStepPath } from "@xyflow/react";
import { LockIcon } from "lucide-react";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { prefersReducedMotion } from "#/lib/motion";
import { cn } from "#/lib/utils";

// The lander's building blocks: pictures of the dashboard and of the servers under it. Canvas cards
// are the product's Card laid out like ServiceNode, in the product's status colours; wires take the
// canvas's own smooth-step edge shape.

/** A status dot in the product's colours (getServiceStatusClasses): quiet once deployed, pink while staged. */
export type Tone = "quiet" | "changed" | "success";

const DOT = {
  quiet: ["bg-muted", "bg-muted-foreground"],
  changed: ["bg-changed-soft", "bg-changed"],
  success: ["bg-success-soft", "bg-success"],
} as const satisfies Record<Tone, readonly [string, string]>;

export function StatusDot({ tone = "quiet" }: { tone?: Tone }) {
  const [outer, inner] = DOT[tone];
  return (
    <span className={cn("flex size-3 shrink-0 items-center justify-center rounded-full", outer)}>
      <span className={cn("size-1.5 rounded-full", inner)} />
    </span>
  );
}

export function ServiceCard({
  id,
  icon,
  name,
  sub,
  status,
  tone = "quiet",
  state,
  className,
}: {
  id?: string;
  icon: ReactNode;
  name: string;
  sub: string;
  status: string;
  tone?: Tone;
  /** The canvas's blue for a node the next Deploy changes, while the change waits for Deploy. */
  state?: "info";
  className?: string;
}) {
  return (
    <Card size="node" state={state} data-node={id} className={cn("h-36 w-full max-w-72 justify-between", className)}>
      <CardHeader>
        <div className="flex items-start gap-3">
          <Avatar>
            <AvatarFallback>{icon}</AvatarFallback>
          </Avatar>
          <div className="min-w-0 flex-1 overflow-hidden">
            <CardTitle>{name}</CardTitle>
            <CardDescription>{sub}</CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        <div className="flex items-center gap-3">
          <StatusDot tone={tone} />
          <span className="min-w-0 flex-1 truncate text-muted-foreground">{status}</span>
        </div>
      </CardContent>
    </Card>
  );
}

// ---- wires ------------------------------------------------------------------------------------------

type Box = { x: number; y: number; w: number; h: number };

/**
 * A line between two [data-node] elements. By default it runs like a canvas edge: up from the top of
 * `from` (the service that is used) to the bottom of `to` (its user), arrow at the user, with packets
 * travelling down it the way requests do. `across` runs it sideways, right edge to left edge, with
 * packets travelling from `from` to `to`.
 */
export type Wire = {
  from: string;
  to: string;
  across?: boolean;
  packets: number;
  seconds: number;
  arrow?: boolean;
  /** Classes for this wire alone, such as hiding it where the layout stacks. */
  className?: string;
};

function wirePath(wire: Wire, a: Box, b: Box) {
  const [d] = wire.across
    ? getBezierPath({ sourceX: a.x + a.w, sourceY: a.y + a.h / 2, sourcePosition: Position.Right, targetX: b.x, targetY: b.y + b.h / 2, targetPosition: Position.Left })
    : getSmoothStepPath({ sourceX: a.x + a.w / 2, sourceY: a.y, sourcePosition: Position.Top, targetX: b.x + b.w / 2, targetY: b.y + b.h + 5, targetPosition: Position.Bottom, borderRadius: 12 });
  return d;
}

/**
 * Draws `wires` between the cards of its parent, which must be `relative isolate`: over the parent's
 * backgrounds, under its [data-node] cards (landing.css lifts those), so packets vanish into what they
 * reach. A card hidden at this breakpoint drops its wires.
 */
export function Wires({ wires }: { wires: readonly Wire[] }) {
  const svg = useRef<SVGSVGElement>(null);
  const [paths, setPaths] = useState<string[]>([]);

  useEffect(() => {
    const scope = svg.current?.parentElement;
    if (!scope) return;
    const measure = () => {
      const frame = scope.getBoundingClientRect();
      const box = (id: string): Box | null => {
        const el = scope.querySelector<HTMLElement>(`[data-node="${id}"]`);
        if (!el || el.offsetParent === null) return null;
        const r = el.getBoundingClientRect();
        return { x: r.left - frame.left, y: r.top - frame.top, w: r.width, h: r.height };
      };
      setPaths(
        wires.map((wire) => {
          const a = box(wire.from);
          const b = box(wire.to);
          return a && b ? wirePath(wire, a, b) : "";
        }),
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(scope);
    scope.querySelectorAll("[data-node]").forEach((el) => observer.observe(el));
    return () => observer.disconnect();
  }, [wires]);

  return (
    <svg ref={svg} aria-hidden className="pointer-events-none absolute inset-0 z-0 size-full overflow-visible">
      {wires.map((wire, i) => {
        const d = paths[i];
        if (!d) return null;
        const { seconds, packets } = wire;
        return (
          <g key={`${wire.from}-${wire.to}`} className={wire.className}>
            <path d={d} className="lp-wire" markerEnd={wire.across || wire.arrow === false ? undefined : "url(#lp-arrow)"} />
            {Array.from({ length: packets }, (_, n) => (
              <Packet key={n} path={d} seconds={seconds} delay={(n / packets) * seconds} reverse={!wire.across} />
            ))}
          </g>
        );
      })}
    </svg>
  );
}

/** A request travelling `path` on a loop, fading in at one end and out at the other. */
export function Packet({ path, seconds, delay, reverse }: { path: string; seconds: number; delay: number; reverse: boolean }) {
  const timing = { dur: `${seconds}s`, begin: `-${delay.toFixed(2)}s`, repeatCount: "indefinite" };
  return (
    <circle className="lp-packet" r={4.5}>
      <animateMotion {...timing} path={path} keyPoints={reverse ? "1;0" : "0;1"} keyTimes="0;1" calcMode="linear" />
      <animate {...timing} attributeName="opacity" values="0;1;1;0" keyTimes="0;0.12;0.88;1" />
    </circle>
  );
}

/** The arrowhead every wire on the page points with; render once. */
export function WireDefs() {
  return (
    <svg aria-hidden className="absolute size-0">
      <defs>
        <marker id="lp-arrow" viewBox="0 0 10 10" refX="5" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
          <path d="M 0 0 L 10 5 L 0 10 z" className="lp-arrow" />
        </marker>
      </defs>
    </svg>
  );
}

// ---- time ---------------------------------------------------------------------------------------------

/**
 * Whether a scene has scrolled into view: undefined while server-rendered (so nothing is hidden without
 * JavaScript), false until it first shows, then true for good. Scenes pass it to `data-inview`, which
 * landing.css keys its one-shot reveals to.
 */
export function useInView<T extends HTMLElement>(): [RefObject<T | null>, boolean | undefined] {
  const ref = useRef<T>(null);
  const [seen, setSeen] = useState<boolean>();
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    setSeen(false);
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting) {
          setSeen(true);
          observer.disconnect();
        }
      },
      { threshold: 0.35 },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return [ref, seen];
}

/**
 * Plays `frames` in a loop while `active`, each for its `ms`. Under reduced motion, or before it
 * starts, it holds `rest`: the frame that tells the story best on its own.
 */
export function useLoop<F extends { ms: number }>(frames: readonly [F, ...F[]], active: boolean, rest: F): F {
  const [index, setIndex] = useState<number>();
  useEffect(() => {
    if (!active || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let current = 0;
    setIndex(0);
    let timer = window.setTimeout(function next() {
      current = (current + 1) % frames.length;
      setIndex(current);
      timer = window.setTimeout(next, frames[current]?.ms);
    }, frames[0].ms);
    return () => window.clearTimeout(timer);
  }, [active, frames]);
  return (index === undefined ? undefined : frames[index]) ?? rest;
}

/**
 * How far the page has scrolled through a pinned section (landing.css's lp-pin, whose first child is the
 * frame that pins): 0 as the frame pins under the header, 1 as it lets go. 0 while server-rendered. Under
 * reduced motion it jumps from 0 to 1 halfway through, so nothing moves with the scroll.
 */
export function usePinProgress<T extends HTMLElement>(): [RefObject<T | null>, number] {
  const ref = useRef<T>(null);
  const [progress, setProgress] = useState(0);
  useEffect(() => {
    const section = ref.current;
    const frame = section?.firstElementChild;
    if (!section || !(frame instanceof HTMLElement)) return;
    let pending = 0;
    const measure = () => {
      pending = 0;
      const travel = section.offsetHeight - frame.offsetHeight;
      const pinnedAt = parseFloat(getComputedStyle(frame).top);
      const p = travel > 0 ? Math.min(1, Math.max(0, (pinnedAt - section.getBoundingClientRect().top) / travel)) : 1;
      setProgress(prefersReducedMotion() ? Math.round(p) : p);
    };
    const schedule = () => {
      pending ||= requestAnimationFrame(measure);
    };
    measure();
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
    return () => {
      cancelAnimationFrame(pending);
      window.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
    };
  }, []);
  return [ref, progress];
}

// ---- props --------------------------------------------------------------------------------------------

export function ServerFace({ name, spec, ip }: { name: string; spec: string; ip: string }) {
  return (
    <div className="lp-face">
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex flex-wrap items-baseline gap-x-2.5">
          <b>{name}</b>
          <span className="lp-muted">{spec}</span>
        </div>
        <span className="lp-muted text-xs">{ip}</span>
      </div>
      <div className="flex flex-col items-end gap-1.5">
        <div className="lp-leds">
          <i />
          <i />
          <i />
        </div>
        <div className="lp-act">
          {Array.from({ length: 8 }, (_, i) => (
            <i key={i} style={{ animationDelay: `${(-i * 0.29).toFixed(2)}s` }} />
          ))}
        </div>
      </div>
    </div>
  );
}

export function Terminal({ title, light, className, children }: { title: ReactNode; light?: boolean; className?: string; children: ReactNode }) {
  return (
    <div className={cn("lp-term", !light && "dark", className)}>
      <div className="lp-term-bar">
        <i />
        <i />
        <i />
        <span>{title}</span>
      </div>
      <div className="lp-term-body">{children}</div>
    </div>
  );
}

/** When the `order`th thing in a scene appears, once the scene scrolls into view: lines, then whatever follows them. */
export function revealDelay(order: number) {
  return `${(0.2 + order * 0.45).toFixed(2)}s`;
}

/** One terminal line. Inside a scene that uses useInView, lines appear in `order` once it shows. */
export function Line({ order = 0, prompt, children, className }: { order?: number; prompt?: string; children?: ReactNode; className?: string }) {
  return (
    <p className={cn("lp-line", className)} style={{ animationDelay: revealDelay(order) }}>
      {prompt ? <span className="lp-prompt">{prompt} </span> : null}
      {children}
    </p>
  );
}

export function BrowserWindow({ url, children }: { url: string; children: ReactNode }) {
  return (
    <div className="lp-browser">
      <div className="lp-browser-bar">
        <i />
        <i />
        <i />
        <span className="lp-browser-url">
          <LockIcon className="size-3 shrink-0 text-success" />
          <span className="truncate">{url}</span>
        </span>
      </div>
      {children}
    </div>
  );
}
