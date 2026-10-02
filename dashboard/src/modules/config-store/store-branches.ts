import type { JsonValue, LiveNode, NamedRow } from "@ployz/sdk";
import { asRecord } from "#/lib/json";
import { settingTitle } from "./catalog";

/** A node as the user names it: `data`, not `volumes.data`. */
export const nodeName = (node: string) => node.startsWith("volumes.") ? node.slice("volumes.".length) : node;

/**
 * A row as reads spell its Setting, `NODE` or `NODE.name` (`web.env.KEY`): how a row the Sync reads give lines up with
 * Details' changes, which name Settings by path. Never sent back: commands take the row.
 */
export const rowPath = (row: NamedRow) => row.name === null ? row.node : `${row.node}.${row.name}`;

/** A value as Details or the Sync dialog shows it: a secret is hidden. */
export function rowText(value: JsonValue): string {
  if (value === null) return "";
  if (Array.isArray(value)) return value.map(rowText).join(", ");
  const record = asRecord(value);
  if (record) return "secret" in record ? "hidden" : JSON.stringify(record);
  return String(value);
}

/** A Setting by its name within its node (`image`, `env.KEY`): a variable's key (`variable`, in monospace) or its title. */
export function settingName(name: string) {
  const title = name.startsWith("env.") ? name.slice("env.".length)
    : name.startsWith("mounts.") ? `Mount of ${name.slice("mounts.".length)}`
    : name === "name" ? "Name" : settingTitle(name) ?? name;
  return { name: title, variable: name.startsWith("env.") };
}

/** A row and the value it offers in words: which setting of which node ("New" for the node itself). */
export function presentRow(row: NamedRow & { value: JsonValue }) {
  return { node: nodeName(row.node), label: row.name === null ? "New" : settingName(row.name).name, after: row.name === null ? "" : rowText(row.value) };
}

/** A Branch's Live Nodes as the canvas draws them, each with the ids of the Services here the Store says read it. */
export function liveNodes(live: readonly LiveNode[], services: ReadonlyArray<{ id: string; name: string }>) {
  return live.map((node) => ({ ...node, usedBy: services.filter((service) => node.used_by.includes(service.name)).map((service) => service.id) }));
}
