import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import type { RegionOption } from "#/modules/machines/server-regions";

const ANY = "any";

/** A choice of region: any region, or one a Server is in, each with its Servers. `taken` regions are listed but can't be picked. */
export function RegionSelect({ options, value, onChange, label, taken = [], allowAny = true, disabled, changed }: {
  options: readonly RegionOption[];
  /** The chosen region's key; null is any region. */
  value: string | null;
  onChange: (key: string | null) => void;
  label: string;
  taken?: readonly string[];
  /** Whether "Any region" is a choice: not for a row of several, which each name one. */
  allowAny?: boolean;
  disabled?: boolean;
  changed?: boolean;
}) {
  const choices = allowAny ? options : options.filter((option) => option.key !== null);
  const shown = choices.find((option) => option.key === value);
  return (
    <Select disabled={disabled} value={value ?? ANY} onValueChange={(next) => {
      const key = next === ANY ? null : next;
      if (next !== null && key !== value) onChange(key);
    }}>
      <SelectTrigger aria-label={label} data-changed={changed ? true : undefined}>
        <SelectValue>{shown?.name ?? value}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        <SelectGroup>
          {choices.map((option) => (
            <SelectItem key={option.key ?? ANY} value={option.key ?? ANY} label={option.name}
              disabled={option.key !== null && option.key !== value && taken.includes(option.key)}>
              <span className="flex flex-col">
                <span>{option.name}</span>
                {option.servers.length > 0 ? <span className="text-xs text-muted-foreground">{option.servers.join(", ")}</span> : null}
              </span>
            </SelectItem>
          ))}
        </SelectGroup>
      </SelectContent>
    </Select>
  );
}
