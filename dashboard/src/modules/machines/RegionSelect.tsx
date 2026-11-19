import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import type { RegionOption } from "#/modules/machines/server-regions";

/** What the select holds for a choice. Any region is its own value, which no region's can equal, whatever its key. */
const ANY = "any";
const choiceOf = (key: string | null) => (key === null ? ANY : `region:${key}`);

/** A choice as its line reads: `Falkenstein · server-1, server-2`, and `Sydney · no Servers` for a region nothing can run in. */
function lineOf({ key, name, servers }: RegionOption) {
  if (key === null) return name;
  return `${name} · ${servers.length > 0 ? servers.join(", ") : "no Servers"}`;
}

/**
 * A choice of region: any region, or one of the regions there are, each with its Servers. A region with no Servers and
 * a `taken` one are listed but can't be picked.
 */
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
  const choices = options.filter((option) => allowAny || option.key !== null);
  const shown = choices.find((option) => option.key === value);
  return (
    <Select disabled={disabled} value={choiceOf(value)} onValueChange={(next) => {
      const picked = choices.find((option) => choiceOf(option.key) === next);
      if (picked !== undefined && picked.key !== value) onChange(picked.key);
    }}>
      <SelectTrigger aria-label={label} data-changed={changed ? true : undefined}>
        <SelectValue>{shown?.name ?? value}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        <SelectGroup>
          {choices.map((option) => (
            <SelectItem key={choiceOf(option.key)} value={choiceOf(option.key)} label={option.name}
              disabled={option.key !== null && option.key !== value && (option.servers.length === 0 || taken.includes(option.key))}>
              {lineOf(option)}
            </SelectItem>
          ))}
        </SelectGroup>
      </SelectContent>
    </Select>
  );
}
