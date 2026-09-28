import { useState } from "react";
import type { BranchOption } from "@ployz/sdk/config";
import { presentRow, type ChangeRow } from "#/modules/branches/branch-review";

type Pick = { ticked: boolean; option?: BranchOption; value: string };

/**
 * A tick per row and a value choice per variable, starting from core's defaults. `sent` is what the server takes: the
 * ticked rows' keys, options and new values ("" for none). Unticking a new node leaves out its settings too.
 */
export function useRowPicks(rows: ChangeRow[], nameOf: (lineage: string) => string) {
  const [edits, setEdits] = useState<Record<string, Partial<Pick>>>({});
  const picks = rows.map((row) => {
    const choice = row.role === "move" ? row.choice : undefined;
    const pick: Pick = { ticked: true, option: choice?.default, value: "", ...edits[row.key] };
    return { row, choice, pick, presented: presentRow(row, nameOf) };
  });
  const edit = (key: string, change: Partial<Pick>) => setEdits((current) => ({ ...current, [key]: { ...current[key], ...change } }));
  const leftOut = new Set(picks.flatMap(({ row, pick, presented }) => row.key.endsWith(":node") && !pick.ticked ? [presented.lineageId] : []));
  const ticked = picks.filter(({ pick, presented }) => pick.ticked && !leftOut.has(presented.lineageId));
  return {
    picks, edit, ticked,
    missing: ticked.find(({ pick }) => pick.option === "new" && !pick.value),
    sent: ticked.map(({ row, pick }) => ({ key: row.key, option: pick.option, value: pick.option === "new" ? pick.value : "" })),
  };
}
