import type { BranchOption, JsonValue, LiveNode, MoveChoice, MovePick, MoveRow } from "@ployz/sdk";
import { asRecord } from "#/lib/json";
import { settingTitle } from "./catalog";

/** One changed setting of one node as a sheet or news row words it: `before` is the receiver's, `after` what would land. */
export type PresentedRow = { key: string; lineageId: string; node: string; label: string; before: string; after: string };

/** A Move row's node: `web` of `web`, `web.image` and `web.env.KEY`. A row that is only a node adds it. */
export const moveRowNode = (row: string) => row.split(".")[0] ?? row;
const isNodeRow = (row: string) => !row.includes(".");

/** A value as a sheet shows it: a secret is hidden, a node row names only the node. */
function moveText(value: JsonValue): string {
  if (value === null) return "";
  if (Array.isArray(value)) return value.map(moveText).join(", ");
  const record = asRecord(value);
  if (record) return "secret" in record ? "hidden" : JSON.stringify(record);
  return String(value);
}

/** A Move row in words: which setting of which node, the receiver's value (`before`) and the one that lands. */
export function presentMoveRow(row: MoveRow): PresentedRow {
  const node = moveRowNode(row.row);
  const path = row.row.slice(node.length + 1);
  const label = isNodeRow(row.row) ? "New"
    : path.startsWith("env.") ? path.slice("env.".length)
    : path.startsWith("mounts.") ? `Mount of ${path.slice("mounts.".length)}`
    : path === "name" ? "Name" : settingTitle(path) ?? path;
  return {
    key: row.row, lineageId: node, node, label,
    before: isNodeRow(row.row) ? "" : moveText(row.into),
    after: isNodeRow(row.row) ? "" : moveText(row.from),
  };
}

/** One row of a Save sheet as the user left it. */
export type SheetPick = { key: string; ticked: boolean; choice?: MoveChoice | null; option?: BranchOption; value: string };

/**
 * The Move picks for what the user ticked. A new node moves whole (`web` picks every change of web), so its variables
 * say only how they land: unticked, `leave_out`. Settings of a node the receiver has move one by one.
 */
export function movePicks(entries: readonly SheetPick[]): MovePick[] {
  const nodes = entries.filter((entry) => isNodeRow(entry.key));
  const whole = new Set(nodes.filter((entry) => entry.ticked).map((entry) => entry.key));
  const leftOut = new Set(nodes.filter((entry) => !entry.ticked).map((entry) => entry.key));
  const picks: MovePick[] = [...whole].map((row) => ({ row }));
  for (const entry of entries) {
    const node = moveRowNode(entry.key);
    if (isNodeRow(entry.key) || leftOut.has(node)) continue;
    const option = entry.choice ? entry.option ?? entry.choice.default : null;
    if (whole.has(node)) {
      if (option) picks.push(choicePick(entry.key, entry.ticked ? option : "leave_out", entry.value));
    } else if (entry.ticked) {
      picks.push(option ? choicePick(entry.key, option, entry.value) : { row: entry.key });
    }
  }
  return picks;
}

const choicePick = (row: string, choice: BranchOption, value: string): MovePick =>
  choice === "new" ? { row, choice, value } : { row, choice };

/** A Branch's Live Nodes as the canvas draws them, each with the ids of the Services here the Store says read it. */
export function liveNodes(live: readonly LiveNode[], services: ReadonlyArray<{ id: string; name: string }>) {
  return live.map((node) => ({ ...node, usedBy: services.filter((service) => node.used_by.includes(service.name)).map((service) => service.id) }));
}
