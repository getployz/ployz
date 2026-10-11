import type { ReactNode } from "react";
import { areaY, crosshair, defineChart, lineY, rect, ruleX, ruleY } from "@tanstack/charts";
import { cursorHost, type ChartCursorController } from "@tanstack/charts/cursor";
import { decorative } from "@tanstack/charts/mark/decorative";
import { Chart } from "@tanstack/charts/react/tooltip";
import { scaleLinear } from "@tanstack/charts/scales/linear";
import { tooltip } from "@tanstack/charts/tooltip";
import { portal } from "@tanstack/charts/tooltip/portal";
import { scaleUtc } from "d3-scale";
import { Card } from "#/components/ui/card";
import { cn } from "#/lib/utils";

export type MetricSeries = { readonly label: string; readonly color: string; readonly values: readonly (number | null)[] };
export type TimeBand = { readonly id: string; readonly from: number; readonly to: number };
type Row = { readonly at: Date; readonly values: readonly (number | null)[] };

const AT_FORMAT = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });

/**
 * One metric as a card: title and latest value on top, a thin line over a soft fill, incident bands, deploy rules and an
 * optional dashed limit. Every chart on the tab shares `cursor`, so hovering one lines them all up.
 */
export function MetricChart({ id, title, value, times, series, format, limit, bands, deploys, from, to, cursor, onPick, renderTooltip, dimmed = false, height = 120 }: {
  id: string;
  title: string;
  value: ReactNode;
  times: readonly number[];
  series: readonly MetricSeries[];
  format: (value: number) => string;
  limit?: number;
  bands: readonly TimeBand[];
  deploys: readonly number[];
  from: number;
  to: number;
  cursor: ChartCursorController<Date, number>;
  /** A click on the chart, with the time under it. */
  onPick: (at: number) => void;
  /** The tooltip for the step at `at`, in place of one line per series. */
  renderTooltip?: (at: number) => ReactNode;
  /** Showing an older read while a newer one loads. */
  dimmed?: boolean;
  height?: number;
}) {
  const rows: Row[] = times.map((at, index) => ({ at: new Date(at), values: series.map((line) => line.values[index] ?? null) }));
  const peak = Math.max(limit ?? 0, ...series.flatMap((line) => line.values.flatMap((point) => point === null ? [] : [point])), 0);
  const top = peak === 0 ? 1 : peak * 1.15;
  const definition = defineChart({
    marks: [
      decorative(rect(bands, { x1: (band) => new Date(band.from), x2: (band) => new Date(band.to), y1: () => 0, y2: () => top, fill: "var(--destructive)", fillOpacity: 0.12, inset: 0 })),
      ...series.map((_, index) => decorative(areaY(rows, { x: "at", y: (row) => row.values[index] ?? null, fill: `url(#fill-${index})` }))),
      ...series.map((line, index) => lineY(rows, { x: "at", y: (row) => row.values[index] ?? null, stroke: line.color, strokeWidth: 1.5, z: () => line.label })),
      ...(limit === undefined ? [] : [ruleY([limit], { stroke: "currentColor", strokeOpacity: 0.45, strokeDasharray: "4 4" })]),
      ruleX(deploys.map((at) => new Date(at)), { stroke: "currentColor", strokeOpacity: 0.35, strokeDasharray: "2 3" }),
      crosshair({ y: false, stroke: "currentColor", strokeOpacity: 0.5, strokeDasharray: "3 3" }),
    ],
    gradients: series.map((line, index) => ({
      id: `fill-${index}`, x1: 0, y1: 1, x2: 0, y2: 0,
      stops: [{ offset: 0, color: line.color, opacity: 0 }, { offset: 1, color: line.color, opacity: 0.22 }],
    })),
    clip: true,
    scales: {
      // Railway's look: no axis lines or tick marks, faint dashed grid, muted labels.
      x: { scale: () => scaleUtc().domain([new Date(from), new Date(to)]), axis: { line: false, ticks: { count: 4, size: 0 } } },
      y: {
        scale: () => scaleLinear().domain([0, top]),
        grid: { strokeOpacity: 0.12, strokeDasharray: "3 3" },
        // With nothing drawn the 0..1 domain would label itself "1 ms", so an empty card gets no labels.
        axis: { line: false, ticks: { count: 3, size: 0, format: peak === 0 ? () => "" : format } },
      },
    },
    focus: "group-x",
    cursor: { use: cursorHost, controller: cursor, mode: "focus", match: "x" },
    tooltip: {
      use: tooltip,
      // The Card clips its children, and the Response time tooltip is taller than the chart.
      portal,
      sticky: false,
      formatGroup: (points) => {
        const row = points[0]?.datum;
        if (!row || !("values" in row)) return "";
        return [AT_FORMAT.format(row.at), ...series.map((line, index) => {
          const point = row.values[index];
          return `${line.label}  ${point === null || point === undefined ? "–" : format(point)}`;
        })].join("\n");
      },
    },
  });
  return (
    <Card size="sm" className="gap-2 px-3">
      <div className="flex items-baseline justify-between gap-3">
        <h3 className="text-sm font-medium">{title}</h3>
        <span className="font-mono text-sm text-muted-foreground tabular-nums">{value}</span>
      </div>
      <Chart definition={definition} height={height} initialWidth={600} idPrefix={id} ariaLabel={title}
        onSelect={(point) => { if (point?.xValue instanceof Date) onPick(point.xValue.getTime()); }}
        renderTooltipBody={renderTooltip === undefined ? undefined
          : ({ primaryPoint, defaultBody }) => primaryPoint?.xValue instanceof Date ? renderTooltip(primaryPoint.xValue.getTime()) : defaultBody}
        className={cn("text-xs text-muted-foreground transition-opacity [--ts-chart-tooltip-background:var(--popover)] [--ts-chart-tooltip-color:var(--popover-foreground)]", dimmed && "opacity-50")} />
      {series.length > 1 ? (
        <div className="flex gap-3 text-xs text-muted-foreground">
          {series.map((line) => (
            <span key={line.label} className="flex items-center gap-1.5">
              <span aria-hidden className="inline-block h-0.5 w-3 rounded-full" style={{ background: line.color }} />
              {line.label}
            </span>
          ))}
        </div>
      ) : null}
    </Card>
  );
}
