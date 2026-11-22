import { useState } from "react";
import { ClockIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Field, FieldError, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "#/components/ui/popover";
import { edgeText, parseEdge, QUICK_RANGES, rangeLabel, resolveRange, type UptimeRange } from "#/modules/uptime/uptime-range";
import { useTimeZone } from "#/utils/time-zone";

type RangePickerProps = {
  range: UptimeRange;
  onRange: (range: UptimeRange) => void;
  /** A range the pointer is on, to read before it is picked. */
  onHover: (range: UptimeRange) => void;
};

/** The tab's time range: a quick range, or From and Until typed as "15m ago", "2026-10-11 14:00" or "now". */
export function RangePicker({ range, onRange, onHover }: RangePickerProps) {
  const timeZone = useTimeZone();
  const [open, setOpen] = useState(false);
  const pick = (next: UptimeRange) => {
    setOpen(false);
    onRange(next);
  };
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger render={<Button variant="outline" size="sm" />}>
        <ClockIcon data-icon="inline-start" />
        {rangeLabel(range, timeZone, Date.now())}
      </PopoverTrigger>
      <PopoverContent align="end" className="w-72">
        <RangeForm range={range} timeZone={timeZone} onRange={pick} onHover={onHover} />
      </PopoverContent>
    </Popover>
  );
}

/** Mounts with each opening, so From and Until start at the range shown. */
function RangeForm({ range, timeZone, onRange, onHover }: RangePickerProps & { timeZone: string }) {
  const shown = { from: edgeText(range.from, timeZone), to: edgeText(range.to, timeZone) };
  const [from, setFrom] = useState(shown.from);
  const [to, setTo] = useState(shown.to);
  const [error, setError] = useState<string | null>(null);
  const apply = () => {
    const now = Date.now();
    // An untouched field keeps its edge, which the text may round to the minute.
    const edges = {
      from: from === shown.from ? range.from : parseEdge(from, timeZone, now),
      to: to === shown.to ? range.to : parseEdge(to, timeZone, now),
    };
    if (edges.from === null) return setError(`Can’t read “${from}”`);
    if (edges.to === null) return setError(`Can’t read “${to}”`);
    const window = resolveRange({ from: edges.from, to: edges.to }, now);
    if (window.from >= window.to) return setError("From must be before Until");
    // Data stops at now, so an Until past it is now.
    onRange({ from: edges.from, to: window.to >= now ? "now" : edges.to });
  };
  return (
    <FieldGroup>
      <FieldSet>
        <FieldLegend variant="label">Quick range</FieldLegend>
        <div className="grid grid-cols-3 gap-1">
          {QUICK_RANGES.map((quick) => {
            const next = { from: quick.from, to: "now" };
            const active = range.from === quick.from && range.to === "now";
            return (
              <Button key={quick.from} variant={active ? "secondary" : "ghost"} size="sm"
                onPointerEnter={() => onHover(next)} onClick={() => onRange(next)}>{quick.label}</Button>
            );
          })}
        </div>
      </FieldSet>
      <form className="contents" onSubmit={(event) => { event.preventDefault(); apply(); }}>
        <Field data-invalid={error !== null || undefined}>
          <FieldLabel htmlFor="range-from">From</FieldLabel>
          <Input id="range-from" value={from} placeholder="15m ago" autoComplete="off"
            onChange={(event) => { setFrom(event.target.value); setError(null); }} />
        </Field>
        <Field data-invalid={error !== null || undefined}>
          <FieldLabel htmlFor="range-to">Until</FieldLabel>
          <Input id="range-to" value={to} placeholder="1h ago, 2026-10-11, now" autoComplete="off"
            onChange={(event) => { setTo(event.target.value); setError(null); }} />
          {error ? <FieldError>{error}</FieldError> : null}
        </Field>
        <Button type="submit" size="sm">Apply</Button>
      </form>
    </FieldGroup>
  );
}
