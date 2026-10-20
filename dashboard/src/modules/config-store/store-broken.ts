import type { BrokenReference } from "@ployz/sdk";

/** Why `reference` is broken, in words: "redis was removed", "redis has no URL". */
export function brokenWhy(reference: BrokenReference) {
  return reference.missing === "service"
    ? `${reference.from ?? "Its Service"} was removed`
    : `${reference.from} has no ${reference.reads}`;
}

/** Where `reference` is written, as Details names a row: "sentry · REDIS_URL", or "sentry · config.yml" for a Config file. */
export function brokenAt(reference: BrokenReference) {
  return "variable" in reference ? `${reference.service} · ${reference.variable}` : `${reference.config} · ${reference.file}`;
}

/** Why each place in `broken` that `key` picks is broken, joined: a Service's variables by key, or a Config's files by name. */
function brokenBy(broken: readonly BrokenReference[], key: (reference: BrokenReference) => string | null) {
  const whys = new Map<string, string[]>();
  for (const reference of broken) {
    const at = key(reference);
    if (at !== null) whys.set(at, [...whys.get(at) ?? [], brokenWhy(reference)]);
  }
  return new Map([...whys].map(([at, reasons]) => [at, reasons.join(", ")]));
}

/** Why each of `service`'s variables is broken, by key. */
export const brokenVariables = (broken: readonly BrokenReference[], service: string): ReadonlyMap<string, string> =>
  brokenBy(broken, (reference) => "variable" in reference && reference.service === service ? reference.variable : null);

/** Why each of `config`'s files is broken, by name. */
export const brokenFiles = (broken: readonly BrokenReference[], config: string): ReadonlyMap<string, string> =>
  brokenBy(broken, (reference) => "file" in reference && reference.config === config ? reference.file : null);
