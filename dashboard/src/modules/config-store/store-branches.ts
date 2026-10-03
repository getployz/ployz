import type { JsonValue, LiveNode, NamedRow } from "@ployz/sdk";
import { Option, Schema } from "effect";
import { asRecord } from "#/lib/json";
import { settingTitle } from "./catalog";

/** A node as the user names it: `data`, not `volumes.data`. */
export const nodeName = (node: string) => node.replace(/^volumes\./, "");

/** A Setting `name`'s value as the Sync dialog and hints show it: a secret is hidden, a source in its own words. */
export function rowText(value: JsonValue, name: string | null): string {
  if (name === "source") return sourceText(value);
  if (value === null) return "";
  if (Array.isArray(value)) return value.map((one) => rowText(one, null)).join(", ");
  const record = asRecord(value);
  if (record) return "secret" in record ? "hidden" : JSON.stringify(record);
  return String(value);
}

/** A Setting by its name within its node (`image`, `env.KEY`): a variable's key (`variable`, in monospace) or its title. */
export function settingName(name: string) {
  const title = name.startsWith("env.") ? name.slice("env.".length)
    : name.startsWith("mounts.") ? `Mount of ${name.slice("mounts.".length)}`
    : name === "name" ? "Name" : name === "source" ? "Source" : settingTitle(name) ?? name;
  return { name: title, variable: name.startsWith("env.") };
}

/** Each field a Service's `source` row holds, by the Setting that sets it: the source changes as one row. */
export const SOURCE_FIELDS = [["image", "image"], ["repository", "repository"], ["rootDir", "rootDir"], ["registryCredential", "credentials"]] as const;

const decodeSource = Schema.decodeUnknownOption(Schema.Struct({
  image: Schema.optional(Schema.String), repository: Schema.optional(Schema.String),
  rootDir: Schema.optional(Schema.String), credentials: Schema.optional(Schema.Boolean),
}));

/** The `source` row's value in words: what it runs from, then a root directory and credentials where it has them. */
export function sourceText(value: JsonValue): string {
  return Option.match(decodeSource(value), {
    onNone: () => value === null ? "" : JSON.stringify(value),
    onSome: ({ image, repository, rootDir, credentials }) =>
      [image ?? repository ?? "None", rootDir && rootDir !== "/" ? `in ${rootDir}` : "", credentials ? "with credentials" : ""]
        .filter(Boolean).join(" "),
  });
}

/** A row and the value it offers in words: which setting of which node ("New" for the node itself). */
export function presentRow(row: NamedRow & { value: JsonValue }) {
  return { node: nodeName(row.node), label: row.name === null ? "New" : settingName(row.name).name, after: row.name === null ? "" : rowText(row.value, row.name) };
}

/** A Branch's Live Nodes as the canvas draws them, each with the ids of the Services here the Store says read it. */
export function liveNodes(live: readonly LiveNode[], services: ReadonlyArray<{ id: string; name: string }>) {
  return live.map((node) => ({ ...node, usedBy: services.filter((service) => node.used_by.includes(service.name)).map((service) => service.id) }));
}
