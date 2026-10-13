import { Meter } from "@base-ui/react/meter";
import { cn } from "#/lib/utils";

const RUNNING_OUT_AT = 0.85;

/** A fraction as a meter's value, 0 to 100. */
export const meterValue = (fraction: number) => Math.min(1, Math.max(0, fraction)) * 100;

/** The bar's indicator, in the warning colour once the fraction is running out. */
export const meterIndicatorClass = (fraction: number) =>
  cn("h-full rounded-full", fraction >= RUNNING_OUT_AT ? "bg-warning" : "bg-foreground/60");

export function MetricMeter({ label, fraction, className }: { label: string; fraction: number; className?: string }) {
  return (
    <Meter.Root value={meterValue(fraction)} aria-label={label} className={cn("flex items-center gap-2 text-xs", className)}>
      <Meter.Track className="h-1.5 w-16 overflow-hidden rounded-full bg-muted">
        <Meter.Indicator className={meterIndicatorClass(fraction)} />
      </Meter.Track>
      <Meter.Value className="font-mono tabular-nums">{(_, number) => `${Math.round(number)}%`}</Meter.Value>
    </Meter.Root>
  );
}
