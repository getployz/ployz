import type { BranchView, JsonValue, LiveNode, NamedRow } from "@ployz/sdk";
import { Option, Schema } from "effect";
import { asRecord } from "#/lib/json";
import { settingTitle } from "./catalog";
import type { StoreResult } from "./store.contract";

/** A node as the user names it: `data`, not `volumes.data`. */
export const nodeName = (node: string) => node.replace(/^(volumes|configs)\./, "");

/** A Setting `name`'s value as the Sync dialog and hints show it: a secret is hidden, a source in its own words. */
export function rowText(value: JsonValue, name: string | null): string {
  if (name === "source") return sourceText(value);
  if (name === "source.branch") return branchText(value);
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
    : name === "name" ? "Name" : name === "source" ? "Source" : name === "source.branch" ? "Branch" : settingTitle(name) ?? name;
  return { name: title, variable: name.startsWith("env.") };
}

/** Each field a Service's `source` row holds, by the Setting that sets it: the source changes as one row. */
export const SOURCE_FIELDS = [["image", "image"], ["repository", "repository"], ["rootDir", "rootDir"], ["registryCredential", "credentials"]] as const;

/**
 * A Service's source as the Store shows it, without its repository's ID, authority or stored version. History holds
 * the credentials themselves; a change row holds them by presence and leaves the branch to its own row.
 */
const Branch = Schema.Union([
  Schema.Struct({ type: Schema.Literal("connected"), name: Schema.String }),
  Schema.Struct({ type: Schema.Literal("disconnected") }),
]);
const decodeBranch = Schema.decodeUnknownOption(Branch);
const branchName = (branch: typeof Branch.Type) => branch.type === "connected" ? branch.name : "no branch";

/** The `source.branch` row's value in words: the branch a repository follows, or none. */
export function branchText(value: JsonValue): string {
  if (value === null) return "";
  return Option.match(decodeBranch(value), { onNone: () => JSON.stringify(value), onSome: branchName });
}

const Source = Schema.Union([
  Schema.Struct({ type: Schema.Literal("image"), image: Schema.String,
    credentials: Schema.optional(Schema.Union([Schema.Boolean, Schema.Struct({ type: Schema.Literals(["none", "configured"]) })])) }),
  Schema.Struct({ type: Schema.Literal("git"), repository: Schema.String, rootDir: Schema.optional(Schema.String),
    branch: Schema.optional(Branch) }),
  Schema.Struct({ type: Schema.Literal("empty"), rootDir: Schema.optional(Schema.String) }),
]);
const decodeSource = Schema.decodeUnknownOption(Source);

const inRoot = (rootDir: string | undefined) => rootDir && rootDir !== "/" ? ` in ${rootDir}` : "";

/** What a source runs from, then the part that may move alone: an image's tag, a repository's branch. */
function sourceParts(source: typeof Source.Type) {
  switch (source.type) {
    case "image": {
      const ref = source.image;
      const at = ref.indexOf("@");
      const colon = ref.lastIndexOf(":");
      const split = at >= 0 ? at : colon > ref.lastIndexOf("/") ? colon + 1 : ref.length;
      const { credentials } = source;
      const configured = credentials === true || (credentials !== undefined && credentials !== false && credentials.type === "configured");
      return { from: ref.slice(0, split), part: ref.slice(split), rest: configured ? " with credentials" : "" };
    }
    case "git":
      if (source.branch === undefined) return { from: source.repository, part: "", rest: inRoot(source.rootDir) };
      return { from: `${source.repository} · `, part: branchName(source.branch), rest: inRoot(source.rootDir) };
    case "empty":
      return { from: "None", part: "", rest: inRoot(source.rootDir) };
  }
}

/** The `source` row's value in words: an image, or a repository and its branch, then a root directory and credentials where it has them. */
export function sourceText(value: JsonValue): string {
  if (value === null) return "";
  return Option.match(decodeSource(value), {
    onNone: () => JSON.stringify(value),
    onSome: (source) => {
      const { from, part, rest } = sourceParts(source);
      return `${from}${part}${rest}`;
    },
  });
}

/** A source change in words, before then after: when only the tag or branch moved, after says just that. */
export function sourceChange(before: JsonValue, after: JsonValue): [string, string] {
  const parts = [before, after].map((value) => value === null ? Option.none() : Option.map(decodeSource(value), sourceParts));
  const [was, now] = parts.map(Option.getOrUndefined);
  const onlyPart = was && now && was.from === now.from && was.rest === now.rest && now.part !== "" && was.part !== now.part;
  return [sourceText(before), onlyPart ? now.part : sourceText(after)];
}

/** A row and the value it offers in words: which setting of which node ("New" for the node itself). */
export function presentRow(row: NamedRow & { value: JsonValue }) {
  if (row.node.startsWith("configs.") && row.name?.startsWith("files.")) {
    return { node: nodeName(row.node), label: row.name.slice("files.".length), after: row.value === null ? "" : "File" };
  }
  return { node: nodeName(row.node), label: row.name === null ? "New" : settingName(row.name).name, after: row.name === null ? "" : rowText(row.value, row.name) };
}

/** The Store answers the branch view only for a Branch; any other Environment's is refused. */
export function isBranch(branch: StoreResult<BranchView>): branch is { ok: true; value: BranchView } {
  return branch.ok;
}

/** A Branch's Live Nodes as the canvas draws them, each with the ids of the Services here the Store says read it. */
export function liveNodes(live: readonly LiveNode[], services: ReadonlyArray<{ id: string; name: string }>) {
  return live.map((node) => ({ ...node, usedBy: services.filter((service) => node.used_by.includes(service.name)).map((service) => service.id) }));
}
