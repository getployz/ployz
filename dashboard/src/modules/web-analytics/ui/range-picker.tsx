import { CalendarIcon } from "lucide-react";
import { useState } from "react";
import { Button } from "#/components/ui/button";
import { Field, FieldError, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "#/components/ui/popover";
import { localDay } from "../local-time";
import { customDays, customRange, PRESET_RANGES, type RangeSpec } from "./analytics-model";

type RangePickerProps = { range: RangeSpec; timezone: string | undefined; onRange: (range: RangeSpec) => void };

function rangeLabel(range: RangeSpec) {
  const preset = PRESET_RANGES.find((candidate) => candidate.spec === range);
  if (preset) return preset.label;
  const days = customDays(range);
  if (days === null) return "30 days";
  return days.from === days.to ? days.from : `${days.from} – ${days.to}`;
}

/** Whole days in the site's zone: a preset, or a first and last day. */
export function RangePicker({ range, timezone, onRange }: RangePickerProps) {
  const [open, setOpen] = useState(false);
  const pick = (next: RangeSpec) => {
    setOpen(false);
    onRange(next);
  };
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger render={<Button variant="outline" size="sm" disabled={timezone === undefined} />}>
        <CalendarIcon data-icon="inline-start" />
        {rangeLabel(range)}
      </PopoverTrigger>
      <PopoverContent align="start" className="w-72">
        {timezone === undefined ? null : <RangeForm range={range} timezone={timezone} onRange={pick} />}
      </PopoverContent>
    </Popover>
  );
}

/** Mounts with each opening, so the days start at the range shown. */
function RangeForm({ range, timezone, onRange }: RangePickerProps & { timezone: string }) {
  const today = localDay(timezone, Date.now());
  const shown = customDays(range) ?? { from: today, to: today };
  const [from, setFrom] = useState(shown.from);
  const [to, setTo] = useState(shown.to);
  const [error, setError] = useState<string | null>(null);
  const apply = () => {
    const next = customRange(from, to);
    if (customDays(next) === null) return setError("The first day must come before the last");
    onRange(next);
  };
  return (
    <FieldGroup>
      <FieldSet>
        <FieldLegend variant="label">Quick range</FieldLegend>
        <div className="grid grid-cols-3 gap-1">
          {PRESET_RANGES.map((preset) => (
            <Button key={preset.spec} variant={range === preset.spec ? "secondary" : "ghost"} size="sm"
              onClick={() => onRange(preset.spec)}>{preset.label}</Button>
          ))}
        </div>
      </FieldSet>
      <form className="contents" onSubmit={(event) => { event.preventDefault(); apply(); }}>
        <Field data-invalid={error !== null || undefined}>
          <FieldLabel htmlFor="analytics-from">From</FieldLabel>
          <Input id="analytics-from" type="date" value={from} max={today}
            onChange={(event) => { setFrom(event.target.value); setError(null); }} />
        </Field>
        <Field data-invalid={error !== null || undefined}>
          <FieldLabel htmlFor="analytics-to">Until</FieldLabel>
          <Input id="analytics-to" type="date" value={to} max={today}
            onChange={(event) => { setTo(event.target.value); setError(null); }} />
          {error ? <FieldError>{error}</FieldError> : null}
        </Field>
        <Button type="submit" size="sm">Apply</Button>
      </form>
    </FieldGroup>
  );
}
