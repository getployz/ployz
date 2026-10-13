import { Meter } from "@base-ui/react/meter";
import { cn } from "#/lib/utils";

const RUNNING_OUT_AT = 0.85;

export function MetricMeter({ label, fraction, className }: { label: string; fraction: number; className?: string }) {
  const value = Math.min(1, Math.max(0, fraction)) * 100;
  return (
    <Meter.Root value={value} aria-label={label} className={cn("flex items-center gap-2 text-xs", className)}>
      <Meter.Track className="h-1.5 w-16 overflow-hidden rounded-full bg-muted">
        <Meter.Indicator className={cn("h-full rounded-full", value >= RUNNING_OUT_AT * 100 ? "bg-warning" : "bg-foreground/60")} />
      </Meter.Track>
      <Meter.Value className="font-mono tabular-nums">{(_, number) => `${Math.round(number)}%`}</Meter.Value>
    </Meter.Root>
  );
}
