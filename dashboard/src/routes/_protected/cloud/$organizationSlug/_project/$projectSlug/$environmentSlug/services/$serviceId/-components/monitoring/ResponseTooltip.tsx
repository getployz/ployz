import { failureTotal, formatMs, passes, regionLabel, type UptimeStep } from "#/modules/uptime/uptime";
import { PHASES, type Phase, type RegionNames, type UptimePoint } from "#/modules/uptime/uptime-status";
import { useTimeZone } from "#/utils/time-zone";
import { atFormat } from "./MetricChart";

const PHASE_LABELS = { dns: "DNS", connect: "Connect", tls: "TLS", ttfb: "First byte" } satisfies Record<Phase, string>;
// One hue per phase so thin neighbours stay apart; First byte keeps the Check line's colour.
const PHASE_COLORS = {
  dns: "var(--chart-6)",
  connect: "var(--chart-1)",
  tls: "var(--chart-4)",
  ttfb: "var(--chart-2)",
} satisfies Record<Phase, string>;

/** "Oct 3, 6:05 PM" for a step of 5 minutes or less, "Oct 3, 6:00 PM – 12:00 AM" for a longer one, in `timeZone`. */
function stepLabel(at: number, end: number, timeZone: string) {
  const start = atFormat(timeZone).format(at);
  if (end - at <= 5 * 60_000) return start;
  return `${start} – ${new Intl.DateTimeFormat("en-US", { hour: "numeric", minute: "2-digit", timeZone }).format(end)}`;
}

function checksLabel(regions: readonly UptimePoint[]) {
  const counts = regions.map((point) => point.probes);
  const low = Math.min(...counts);
  const high = Math.max(...counts);
  return `${low === high ? low : `${low}–${high}`} ${high === 1 ? "check" : "checks"} each`;
}

/** Mean time a passing probe spent in each phase. */
const phaseMeans = (point: UptimePoint) => PHASES.map((phase) => ({ phase, ms: point.phaseMsSum[phase] / Math.max(1, passes(point)) }));
const meanTotal = (point: UptimePoint) => phaseMeans(point).reduce((sum, part) => sum + part.ms, 0);

/** One step of the Response time chart, ending at `end`, region by region: how fast, where the time went, and what failed. */
export function ResponseTooltip({ step, end, edgeMs, regions }: {
  step: UptimeStep;
  end: number;
  edgeMs: number | null;
  regions: RegionNames;
}) {
  const timeZone = useTimeZone();
  const slowest = Math.max(...step.regions.map(meanTotal), 0);
  return (
    <div className="flex w-64 flex-col gap-2 rounded-lg bg-popover p-2.5 text-popover-foreground shadow-md ring-1 ring-foreground/10">
      <div className="flex items-baseline justify-between gap-3">
        <span className="font-medium">{stepLabel(step.at, end, timeZone)}</span>
        {step.regions.length > 0 ? <span className="text-muted-foreground tabular-nums">{checksLabel(step.regions)}</span> : null}
      </div>
      {step.regions.length === 0 ? <span className="text-muted-foreground">No checks</span> : null}
      {step.regions.map((point) => <RegionRow key={point.region} point={point} slowest={slowest} regions={regions} />)}
      {edgeMs === null ? null : (
        <div className="flex justify-between gap-3">
          <span className="text-muted-foreground">Edge</span>
          <span className="tabular-nums">{formatMs(edgeMs)}</span>
        </div>
      )}
      {slowest > 0 ? (
        <div className="flex flex-wrap gap-x-3 gap-y-1 text-muted-foreground">
          {PHASES.map((phase) => (
            <span key={phase} className="flex items-center gap-1.5">
              <span aria-hidden className="inline-block size-2 rounded-full" style={{ background: PHASE_COLORS[phase] }} />
              {PHASE_LABELS[phase]}
            </span>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function RegionRow({ point, slowest, regions }: { point: UptimePoint; slowest: number; regions: RegionNames }) {
  const label = regionLabel(regions, point.region);
  const failed = failureTotal(point.failures);
  const single = point.probes === 1;
  // Most probes failed: the row leads with the failure, not with a median of the few that passed.
  const failing = failed * 2 > point.probes;
  const error = point.lastError?.text ?? null;
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-3">
        {label ? <span className={label.named ? undefined : "font-mono"}>{label.text}</span> : null}
        {failing ? (
          <span className="text-destructive tabular-nums">{single ? "Down" : `${failed} of ${point.probes} failed`}</span>
        ) : (
          <span className="tabular-nums">
            {point.medianMs === null ? "–" : formatMs(point.medianMs)}
            {single || point.maxMs === null ? null : <span className="text-muted-foreground"> · max {formatMs(point.maxMs)}</span>}
          </span>
        )}
      </div>
      {passes(point) > 0 && slowest > 0 ? (
        <div className="flex h-1.5 overflow-hidden rounded-full" style={{ width: `${(meanTotal(point) / slowest) * 100}%` }}>
          {phaseMeans(point).map(({ phase, ms }) => (
            <span key={phase} className="h-full" style={{ flexGrow: ms, background: PHASE_COLORS[phase] }} />
          ))}
        </div>
      ) : null}
      {failed > 0 && error !== null ? (
        <span className="text-destructive tabular-nums">{failing ? error : `${failed} of ${point.probes} failed · ${error}`}</span>
      ) : null}
    </div>
  );
}
