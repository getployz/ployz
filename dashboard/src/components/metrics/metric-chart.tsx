import { useEffect, useState, type ReactNode } from "react";
import { areaY, crosshair, defineChart, dot, lineY, rect, ruleY } from "@tanstack/charts";
import { createChartCursor, cursorHost, type ChartCursorController } from "@tanstack/charts/cursor";
import { decorative } from "@tanstack/charts/mark/decorative";
import { Chart } from "@tanstack/charts/react/tooltip";
import { scaleLinear } from "@tanstack/charts/scales/linear";
import { tooltip } from "@tanstack/charts/tooltip";
import { portal } from "@tanstack/charts/tooltip/portal";
import { scaleUtc } from "d3-scale";
import { Button } from "#/components/ui/button";
import { Card } from "#/components/ui/card";
import { cn } from "#/lib/utils";
import { useTimeZone } from "#/utils/time-zone";

export type MetricSeries = {
  readonly label: string;
  readonly color: string;
  readonly values: readonly (number | null)[];
  readonly dashed?: boolean;
  readonly gaps?: readonly TimeSpan[];
};
export type TimeSpan = { readonly from: number; readonly to: number };
export type TimeBand = TimeSpan & { readonly id: string };
export type ChartMarker = { readonly at: number; readonly kind: "oom" | "exit" | "restart"; readonly label: string };
export type ChartLimit = { readonly value: number; readonly label: string };
export type MetricCursor = ChartCursorController<Date, number>;
type Row = { readonly at: Date; readonly values: readonly (number | null)[] };

const MARGIN = { top: 10, right: 8, bottom: 20, left: 62 } as const;

export const atFormat = (timeZone: string) =>
  new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", hour: "numeric", minute: "2-digit", timeZone });

/**
 * One cursor for a page of charts, released on Escape or a tap outside every chart and tooltip, since the library leaves a
 * pinned shared cursor up on outside clicks.
 */
export function useMetricCursor(): MetricCursor {
  const [cursor] = useState(() => createChartCursor<Date, number>());
  useEffect(() => {
    const release = (event: PointerEvent | KeyboardEvent) => {
      if (!cursor.getState()?.pinned) return;
      if (event instanceof KeyboardEvent ? event.key !== "Escape"
        : event.target instanceof Element && event.target.closest("[data-metric-chart], .metric-tooltip")) return;
      cursor.setState(null);
    };
    document.addEventListener("pointerdown", release, true);
    document.addEventListener("keydown", release);
    return () => {
      document.removeEventListener("pointerdown", release, true);
      document.removeEventListener("keydown", release);
    };
  }, [cursor]);
  return cursor;
}

const isolated = (values: readonly (number | null)[], index: number) =>
  values[index] != null && values[index - 1] == null && values[index + 1] == null;

const inSpan = (at: number, spans: readonly TimeSpan[]) => spans.some((span) => at >= span.from && at < span.to);

const MARKER_GLYPHS = {
  oom: "size-0 border-x-4 border-b-[7px] border-x-transparent border-b-destructive",
  exit: "size-1.5 bg-destructive",
  restart: "size-2 rounded-full border border-current bg-background",
} as const satisfies Record<ChartMarker["kind"], string>;

/** A time tick nearer an edge than this share of the plot runs into the y labels or past the card. */
const EDGE = 0.06;

export function MetricChart({
  id, title, subtitle, value, times, series, format, axisFormat, limit, bands = [], gaps = [], markers = [], deploys, from, to, cursor,
  incidentAt, renderTooltip, focusAt = [], dimmed = false, height = 120, empty = "No readings",
}: {
  id: string;
  title: string;
  subtitle?: string;
  value: ReactNode;
  times: readonly number[];
  series: readonly MetricSeries[];
  format: (value: number) => string;
  /** The y labels' formatter for an axis that tops out at `top`, so every label reads in one unit. */
  axisFormat?: (top: number) => (value: number) => string;
  limit?: ChartLimit | null;
  bands?: readonly TimeBand[];
  gaps?: readonly TimeSpan[];
  markers?: readonly ChartMarker[];
  deploys: readonly number[];
  from: number;
  to: number;
  cursor: MetricCursor;
  incidentAt?: (at: number) => ((x: number, y: number) => void) | null;
  renderTooltip?: (at: number) => ReactNode;
  focusAt?: readonly number[];
  dimmed?: boolean;
  height?: number;
  empty?: string | null;
}) {
  const timeZone = useTimeZone();
  const at = atFormat(timeZone);
  const tick = new Intl.DateTimeFormat("en-US", to - from <= 86_400_000
    ? { hour: "numeric", minute: "2-digit", timeZone }
    : { month: "short", day: "numeric", timeZone });
  const legend = series.filter((line, index) => series.findIndex((other) => other.label === line.label) === index);
  const rows: Row[] = times.map((at, index) => ({ at: new Date(at), values: series.map((line) => line.values[index] ?? null) }));
  const focusRows = rows.filter((row) => focusAt.includes(row.at.getTime()));
  const present = series.flatMap((line) => line.values.flatMap((point) => point === null ? [] : [point]));
  const peak = Math.max(limit?.value ?? 0, ...present, 0);
  const drawsAnything = peak > 0;
  const top = drawsAnything ? peak * 1.15 : 1;
  const plotHeight = height - MARGIN.top - MARGIN.bottom;
  const span = Math.max(1, to - from);
  const fraction = (time: number) => Math.min(1, Math.max(0, (time - from) / span));
  const xTicks = scaleUtc().domain([new Date(from), new Date(to)]).ticks(4)
    .filter((tickAt) => fraction(tickAt.getTime()) >= EDGE && fraction(tickAt.getTime()) <= 1 - EDGE);
  const definition = defineChart({
    marks: [
      decorative(rect(bands, { x1: (band) => new Date(band.from), x2: (band) => new Date(band.to), y1: () => 0, y2: () => top, fill: "var(--destructive)", fillOpacity: 0.12, inset: 0 })),
      ...series.flatMap((line, index) => line.dashed ? [] : [decorative(areaY(rows, { x: "at", y: (row) => row.values[index] ?? null, fill: `url(#fill-${index})` }))]),
      ...series.map((line, index) => lineY(rows, {
        x: "at", y: (row) => row.values[index] ?? null, stroke: line.color, strokeWidth: 1.5, z: () => line.label,
        strokeDasharray: line.dashed ? "4 3" : undefined,
      })),
      ...series.map((line, index) => decorative(dot(rows.filter((_, step) => isolated(line.values, step)),
        { x: "at", y: (row) => row.values[index] ?? null, r: 2, fill: line.color }))),
      lineY(focusRows, { x: "at", y: () => 0, strokeOpacity: 0, z: () => "focus" }),
      ...(limit ? [ruleY([limit.value], { stroke: "currentColor", strokeOpacity: 0.45, strokeDasharray: "4 4" })] : []),
      crosshair({ y: false, stroke: "currentColor", strokeOpacity: 0.5, strokeDasharray: "3 3" }),
    ],
    gradients: series.map((line, index) => ({
      id: `fill-${index}`, x1: 0, y1: 1, x2: 0, y2: 0,
      stops: [{ offset: 0, color: line.color, opacity: 0 }, { offset: 1, color: line.color, opacity: 0.22 }],
    })),
    clip: true,
    margin: MARGIN,
    scales: {
      x: { scale: () => scaleUtc().domain([new Date(from), new Date(to)]), axis: { line: false, ticks: { values: xTicks, size: 0, format: (value: Date) => tick.format(value) } } },
      y: {
        scale: () => scaleLinear().domain([0, top]),
        grid: { strokeOpacity: 0.12, strokeDasharray: "3 3" },
        axis: { line: false, ticks: { count: 3, size: 0, format: drawsAnything ? axisFormat?.(top) ?? format : () => "" } },
      },
    },
    focus: "group-x",
    cursor: { use: cursorHost, controller: cursor, mode: "focus", match: "x", pin: true },
    tooltip: {
      use: tooltip,
      // In the top layer, so the card's overflow-hidden never cuts a tall tooltip off.
      portal,
      anchor: { x: "group-center", y: "plot-top" },
      placement: ["right", "left", "bottom"],
      className: "metric-tooltip",
      formatGroup: (points) => {
        const row = points[0]?.datum;
        if (!row || !("values" in row)) return "";
        const missing = (line: MetricSeries) => inSpan(row.at.getTime(), line.gaps ?? gaps) ? "Not measured" : "–";
        return [at.format(row.at), ...series.map((line, index) => {
          const point = row.values[index];
          return `${line.label}  ${point === null || point === undefined ? missing(line) : format(point)}`;
        })].join("\n");
      },
    },
  });
  const plotX = (part: number) => `calc(${MARGIN.left}px + ${part} * (100% - ${MARGIN.left + MARGIN.right}px))`;
  const shownGaps = gaps.filter((gap) => gap.to > from && gap.from < to);
  const blank = empty !== null && present.length === 0 && shownGaps.length === 0 && focusRows.length === 0;
  return (
    <Card size="sm" className="gap-2 px-3" data-metric-chart>
      <div className="flex items-baseline justify-between gap-3">
        <div className="flex items-baseline gap-2">
          <h3 className="text-sm font-medium">{title}</h3>
          {subtitle ? <span className="text-xs text-muted-foreground">{subtitle}</span> : null}
        </div>
        <span className="font-mono text-sm text-muted-foreground tabular-nums">{value}</span>
      </div>
      <div className="relative">
        <Chart definition={definition} height={height} initialWidth={600} idPrefix={id} ariaLabel={title}
          renderTooltipBody={({ primaryPoint, defaultBody, pinned, dismiss }) => {
            if (!(primaryPoint?.xValue instanceof Date)) return defaultBody;
            const time = primaryPoint.xValue.getTime();
            const open = pinned ? incidentAt?.(time) ?? null : null;
            return (
              <div className="rounded-lg bg-popover p-2.5 text-xs text-popover-foreground shadow-md ring-1 ring-foreground/10">
                {renderTooltip?.(time) ?? <div className="whitespace-pre">{defaultBody}</div>}
                {open ? (
                  <Button variant="link" size="sm" className="h-auto px-0 pt-1"
                    onClick={(event) => { dismiss(); open(event.clientX, event.clientY); }}>
                    Incident details
                  </Button>
                ) : null}
              </div>
            );
          }}
          className={cn("text-xs text-muted-foreground transition-opacity", dimmed && "opacity-50")} />
        <div aria-hidden={markers.length === 0 || undefined} className="pointer-events-none absolute inset-0 text-xs text-muted-foreground">
          {shownGaps.map((gap) => {
            const width = fraction(gap.to) - fraction(gap.from);
            return (
              <div key={`${gap.from}-${gap.to}`} className="absolute flex items-center justify-center overflow-hidden bg-muted/40"
                style={{
                  left: plotX(fraction(gap.from)), width: `calc(${width} * (100% - ${MARGIN.left + MARGIN.right}px))`, top: MARGIN.top, height: plotHeight,
                  backgroundImage: "repeating-linear-gradient(135deg, var(--border) 0 1px, transparent 1px 6px)",
                }}>
                {width >= 0.12 ? <span className="whitespace-nowrap rounded-sm bg-background/80 px-1">Not measured</span> : null}
              </div>
            );
          })}
          {deploys.filter((at) => at >= from && at <= to).map((at) => (
            <div key={at} data-deploy className="absolute border-l border-dashed border-current opacity-50"
              style={{ left: plotX(fraction(at)), top: MARGIN.top, height: plotHeight }} />
          ))}
          {blank ? (
            <div className="absolute flex items-center justify-center" style={{ left: MARGIN.left, right: MARGIN.right, top: MARGIN.top, height: plotHeight }}>
              {empty}
            </div>
          ) : null}
          {limit && peak > 0 ? (
            <span className="absolute whitespace-nowrap" style={{ right: MARGIN.right, top: MARGIN.top + plotHeight * (1 - limit.value / top) - 16 }}>
              {limit.label}
            </span>
          ) : null}
          {markers.filter((marker) => marker.at >= from && marker.at <= to).map((marker) => (
            <span key={`${marker.kind}-${marker.at}-${marker.label}`} role="img" aria-label={marker.label} title={marker.label}
              className={cn("pointer-events-auto absolute -translate-x-1/2", MARKER_GLYPHS[marker.kind])}
              style={{ left: plotX(fraction(marker.at)), top: MARGIN.top + plotHeight - (marker.kind === "oom" ? 18 : 9) }} />
          ))}
        </div>
      </div>
      {legend.length > 1 ? (
        <div className="flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground">
          {legend.map((line) => (
            <span key={line.label} className="flex items-center gap-1.5">
              <span aria-hidden className="inline-block w-3 border-t-2" style={{ borderColor: line.color, borderStyle: line.dashed ? "dashed" : "solid" }} />
              {line.label}
            </span>
          ))}
        </div>
      ) : null}
    </Card>
  );
}
