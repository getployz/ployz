import type { ReferenceTarget } from "#/modules/variables/variable-autocomplete";
import { KEY_SRC, SLUG_SRC } from "#/modules/variables/variable-template";

/** A closed `${{ }}` token in a Config file, at `[from, to)`: a reference that resolves, or why it doesn't. */
export type ConfigReference = { from: number; to: number } & (
  | { kind: "ref"; service: string; key: string; secret: boolean }
  | { kind: "unknown"; message: string }
);

/** What Preview shows for `service.KEY`: its value, or that it is secret. */
export type ReferenceValue = { secret: true } | { secret: false; value: string };

export type PreviewSegment =
  | { kind: "text"; text: string }
  | { kind: "value"; text: string }
  | { kind: "secret" }
  | { kind: "unknown"; text: string };

type Token = { from: number; to: number } & (
  | { kind: "escape" }
  | { kind: "token"; service: string | null; key: string }
);

const TOKEN = new RegExp(`\\$\\$\\{\\{|\\$\\{\\{\\s*(?:(${SLUG_SRC})\\.)?(${KEY_SRC})\\s*\\}\\}`, "g");

function* tokens(text: string): Generator<Token> {
  for (const match of text.matchAll(TOKEN)) {
    const from = match.index;
    const to = from + match[0].length;
    const [, service, key] = match;
    yield key === undefined ? { from, to, kind: "escape" } : { from, to, kind: "token", service: service ?? null, key };
  }
}

/** Every closed reference in `text`; an unclosed `${{` and the `$${{` escape yield nothing. */
export function configReferences(text: string, targets: readonly ReferenceTarget[], services: readonly string[]): ConfigReference[] {
  const keys = new Map<string, Map<string, boolean>>();
  for (const target of targets) {
    if (target.ownerSlug === null) continue;
    const owned = keys.get(target.ownerSlug) ?? new Map<string, boolean>();
    owned.set(target.key, target.isSecret);
    keys.set(target.ownerSlug, owned);
  }
  const known = new Set(services);

  return [...tokens(text)].flatMap((token): ConfigReference[] => {
    if (token.kind === "escape") return [];
    const { from, to, service, key } = token;
    if (service === null) return [{ from, to, kind: "unknown", message: "Configs are shared. Write ${{ service.KEY }}." }];
    if (!known.has(service)) {
      return [{ from, to, kind: "unknown", message: `Unknown service ${service}${didYouMean(service, services)}` }];
    }
    const owned = keys.get(service);
    const secret = owned?.get(key);
    if (secret === undefined) {
      return [{ from, to, kind: "unknown", message: `${service} has no ${key}${didYouMean(key, [...(owned?.keys() ?? [])])}` }];
    }
    return [{ from, to, kind: "ref", service, key, secret }];
  });
}

/** `text` as Preview renders it, with `values` keyed `service.KEY`; a secret segment never carries its value. */
export function previewSegments(text: string, values: ReadonlyMap<string, ReferenceValue>): PreviewSegment[] {
  const segments: PreviewSegment[] = [];
  const pushText = (piece: string) => {
    if (piece === "") return;
    const last = segments.at(-1);
    if (last?.kind === "text") last.text += piece;
    else segments.push({ kind: "text", text: piece });
  };

  let at = 0;
  for (const token of tokens(text)) {
    pushText(text.slice(at, token.from));
    at = token.to;
    if (token.kind === "escape") {
      pushText("${{");
      continue;
    }
    const value = token.service === null ? undefined : values.get(`${token.service}.${token.key}`);
    if (value === undefined) segments.push({ kind: "unknown", text: text.slice(token.from, token.to) });
    else if (value.secret) segments.push({ kind: "secret" });
    else segments.push({ kind: "value", text: value.value });
  }
  pushText(text.slice(at));
  return segments;
}

function didYouMean(name: string, candidates: readonly string[]): string {
  let best: { candidate: string; distance: number } | null = null;
  for (const candidate of candidates) {
    const distance = optimalStringAlignmentDistance(name.toLowerCase(), candidate.toLowerCase());
    const close = distance <= Math.max(1, Math.floor(name.length / 3)) || sharedPrefix(name, candidate) >= 3;
    if (close && (best === null || distance < best.distance)) best = { candidate, distance };
  }
  return best === null ? "" : ` · Did you mean ${best.candidate}?`;
}

function sharedPrefix(left: string, right: string): number {
  let length = 0;
  while (length < left.length && left[length]?.toLowerCase() === right[length]?.toLowerCase()) length += 1;
  return length;
}

function optimalStringAlignmentDistance(left: string, right: string): number {
  const rows: number[][] = [Array.from({ length: right.length + 1 }, (_, index) => index)];
  for (let i = 1; i <= left.length; i += 1) {
    const row = [i];
    for (let j = 1; j <= right.length; j += 1) {
      const above = rows[i - 1] ?? [];
      const cost = left[i - 1] === right[j - 1] ? 0 : 1;
      let distance = Math.min((above[j] ?? 0) + 1, (row[j - 1] ?? 0) + 1, (above[j - 1] ?? 0) + cost);
      if (i > 1 && j > 1 && left[i - 1] === right[j - 2] && left[i - 2] === right[j - 1]) {
        distance = Math.min(distance, (rows[i - 2]?.[j - 2] ?? 0) + 1);
      }
      row.push(distance);
    }
    rows.push(row);
  }
  return rows[left.length]?.[right.length] ?? 0;
}
