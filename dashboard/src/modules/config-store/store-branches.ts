import type { JsonValue, LiveNode } from "@ployz/sdk";
import { asRecord } from "#/lib/json";
import { settingTitle } from "./catalog";

/** One changed setting of one node in words: `before` is the receiver's, `after` what would land. */
export type PresentedRow = { key: string; lineageId: string; node: string; label: string; before: string; after: string };

/** How many parts of a row name its node: a Volume's is `volumes.NAME`, a Service's its name. */
const nodeParts = (row: string) => row.startsWith("volumes.") ? 2 : 1;
/**
 * A row's node: `web` of `web`, `web.image` and `web.env.KEY`; `volumes.data` of `volumes.data.size`. A row that is
 * only a node adds it.
 */
const rowNode = (row: string) => row.split(".").slice(0, nodeParts(row)).join(".");
const isNodeRow = (row: string) => row.split(".").length === nodeParts(row);
/** A node as the user names it: `data`, not `volumes.data`. */
export const nodeName = (node: string) => node.startsWith("volumes.") ? node.slice("volumes.".length) : node;

/** A value as Details or the Sync dialog shows it: a secret is hidden. */
export function rowText(value: JsonValue): string {
  if (value === null) return "";
  if (Array.isArray(value)) return value.map(rowText).join(", ");
  const record = asRecord(value);
  if (record) return "secret" in record ? "hidden" : JSON.stringify(record);
  return String(value);
}

/** A setting (`NODE.path`) by name within its node: a variable's key (`variable`, in monospace) or a Setting's title. */
export function settingName(path: string) {
  const field = path.slice(rowNode(path).length + 1);
  const name = field.startsWith("env.") ? field.slice("env.".length)
    : field.startsWith("mounts.") ? `Mount of ${field.slice("mounts.".length)}`
    : field === "name" ? "Name" : settingTitle(field) ?? field;
  return { name, variable: field.startsWith("env.") };
}

/** A row (`NODE[.path]`) in words: which setting of which node, the receiver's value (`before`) and the one that lands. */
export function presentRow(row: { row: string; from: JsonValue; into: JsonValue }): PresentedRow {
  const node = rowNode(row.row);
  return {
    key: row.row, lineageId: node, node: nodeName(node), label: isNodeRow(row.row) ? "New" : settingName(row.row).name,
    before: isNodeRow(row.row) ? "" : rowText(row.into),
    after: isNodeRow(row.row) ? "" : rowText(row.from),
  };
}

/** A Branch's Live Nodes as the canvas draws them, each with the ids of the Services here the Store says read it. */
export function liveNodes(live: readonly LiveNode[], services: ReadonlyArray<{ id: string; name: string }>) {
  return live.map((node) => ({ ...node, usedBy: services.filter((service) => node.used_by.includes(service.name)).map((service) => service.id) }));
}
