import type { DestructiveEffect, JsonValue } from "@ployz/sdk";

/** The operations `ployz` and agents run through Cloud that may need a person's yes. Upgrade and a Service start never do. */
export type OperationVerb = "drain" | "set" | "clean" | "remove" | "forget" | "release" | "mirror" | "sync" | "remove_mirror" | "stop" | "restart" | "unprotect" | "move" | "restore" | "resize" | "set_cluster_name" | "set_ingress_doors" | "set_private_access";

/** An operation as a person says yes to it: what it acts on, its verb and name, and the preview and effects its digest binds. */
export type OperationAsked = {
  /** `<kind>:<key>`, such as `server:<machine id>` or `service:<environment id>/<name>`. */
  subject: string;
  verb: OperationVerb;
  name: string;
  preview: JsonValue;
  effects: DestructiveEffect[];
  /** The Protected Environment it lands in when its caller can't protect it: an owner or admin does it instead. */
  protected?: string;
  /** It asks from the CLI though it destroys nothing, as a Cluster rename does: old private names stop answering. */
  alwaysAsks?: true;
  /** The `ployz` command a person runs to do it, which an agent's refusal names. */
  ployz?: string;
};

/** The verbs that don't read as their own name: a card's title for each, and how a refusal says what was asked. */
export const OPERATION_WORDS = {
  set_cluster_name: { title: "Rename cluster", asked: (name: string) => `renaming the Cluster to ${name}` },
  set_ingress_doors: { title: "Switch web traffic", asked: (name: string) => `switching web traffic ${name}` },
  set_private_access: { title: "Turn private access", asked: (name: string) => `turning Private Access ${name}` },
} as const satisfies Partial<Record<OperationVerb, { title: string; asked: (name: string) => string }>>;

export const operationWords = (verb: OperationVerb) => Object.entries(OPERATION_WORDS).find(([key]) => key === verb)?.[1] ?? null;

/** What a refusal calls an operation: "renaming the Cluster to acme", or "this drain of fra-1". */
export const operationAsked = (verb: OperationVerb, name: string) =>
  operationWords(verb)?.asked(name) ?? `this ${verb.replaceAll("_", " ")} of ${name}`;
