import { useState } from "react";
import type { BranchOption } from "@ployz/sdk/config";
import { Checkbox } from "#/components/ui/checkbox";
import { Input } from "#/components/ui/input";
import { ItemGroup } from "#/components/ui/item";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { presentRow, type ChangeRow } from "#/modules/branches/branch-review";
import { ChangeRowItem } from "./ChangeRowItem";

type Pick = { ticked: boolean; option?: BranchOption; value: string };

/**
 * A tick per row and a value choice per variable, starting from core's defaults. `sent` is what the server takes: the
 * ticked rows' keys, options and new values ("" for none).
 */
export function useRowPicks(rows: ChangeRow[], nameOf: (lineage: string) => string) {
  const [edits, setEdits] = useState<Record<string, Partial<Pick>>>({});
  const picks = rows.map((row) => {
    const choice = row.role === "move" ? row.choice : undefined;
    const pick: Pick = { ticked: true, option: choice?.default, value: "", ...edits[row.key] };
    return { row, choice, pick, presented: presentRow(row, nameOf) };
  });
  const edit = (key: string, change: Partial<Pick>) => setEdits((current) => ({ ...current, [key]: { ...current[key], ...change } }));
  const ticked = picks.filter(({ pick }) => pick.ticked);
  return {
    picks, edit, ticked,
    missing: ticked.find(({ pick }) => pick.option === "new" && !pick.value),
    sent: ticked.map(({ row, pick }) => ({ key: row.key, option: pick.option, value: pick.option === "new" ? pick.value : "" })),
  };
}

/** The rows with their ticks and value choices. `names` words each option; `verb` labels the tick. */
export function RowPicks({ picks: { picks, edit }, names, verb }: {
  picks: ReturnType<typeof useRowPicks>;
  names: { from: string; parent: string; destination: string };
  verb: string;
}) {
  return (
    <ItemGroup className="gap-1">
      {picks.map(({ row, choice, pick, presented }) => (
        <div key={row.key} className="flex flex-col gap-1">
          <ChangeRowItem row={presented} conflict={row.role === "move" && row.conflict ? names.destination : undefined}>
            <Checkbox checked={pick.ticked} onCheckedChange={(checked) => edit(row.key, { ticked: checked === true })}
              aria-label={`${verb} ${presented.node}${presented.label ? ` · ${presented.label}` : ""}`} />
          </ChangeRowItem>
          {choice && pick.ticked ? (
            <div className="flex flex-wrap gap-1">
              <NativeSelect aria-label={`Value of ${presented.label}`} value={pick.option}
                // SAFETY: the options are exactly choice.options.
                onChange={(event) => edit(row.key, { option: event.target.value as BranchOption })}>
                {choice.options.map((option) => (
                  <NativeSelectOption key={option} value={option}>
                    {option === "from" ? `${names.from}'s value` : option === "parent" ? `${names.parent}'s value`
                      : option === "new" ? `A new value for ${names.destination}` : "Leave it out"}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
              {pick.option === "new" ? (
                <Input className="min-w-48 flex-1" type={choice.secret ? "password" : "text"} autoComplete="off" value={pick.value}
                  aria-label={`New value of ${presented.label}`}
                  placeholder={`${names.destination}'s value`}
                  onChange={(event) => edit(row.key, { value: event.target.value })} />
              ) : null}
            </div>
          ) : null}
        </div>
      ))}
    </ItemGroup>
  );
}
