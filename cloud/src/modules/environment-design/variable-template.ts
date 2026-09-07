import type { ValuePart, ValuePartRefOwner } from "#/modules/environment-design/tables";

/**
 * Pure, isomorphic helpers for the variable-templating grammar. Shared by the
 * client (autocomplete + display) and the server (deploy resolver + edge
 * derivation), so this module must not import anything `server-only`.
 *
 * Two representations exist:
 *  - **parts** (`ValuePart[]`) — canonical, persisted form.
 *  - **display string** — `${{ }}`-style text used in inputs, the raw editor,
 *    and copy. Owner refs render with the producer's current slug; a literal
 *    `${{` inside a text part is escaped as `$${{` so the string round-trips.
 *
 * Display grammar:
 *  - `${{ KEY }}`            — self ref (owner's own scope)
 *  - `${{ slug.KEY }}`       — ref to service/group `slug`'s exported `KEY`
 *  - `$${{`                  — a literal `${{`
 */

const KEY_SRC = "[A-Za-z_][A-Za-z0-9_]*";
const SLUG_SRC = "[A-Za-z0-9][A-Za-z0-9_-]*";

// Anchored matcher for a single `${{ [slug.]KEY }}` token at the start of a string.
const TOKEN_AT_START = new RegExp(
  `^\\$\\{\\{\\s*(?:(${SLUG_SRC})\\.)?(${KEY_SRC})\\s*\\}\\}`,
);

// Global matcher for scanning a display string for `${{ [slug.]KEY }}` tokens.
const TOKEN_GLOBAL = new RegExp(
  `\\$\\{\\{\\s*(?:(${SLUG_SRC})\\.)?(${KEY_SRC})\\s*\\}\\}`,
  "g",
);

export type ParsedRef = { owner: ValuePartRefOwner; key: string };

export type LookupSlug = (lineageId: string) => string | null;
export type LookupLineage = (
  slug: string,
) => { lineageId: string; scope: "service" | "variable_group" } | null;

export type ParseDisplayResult = {
  parts: ValuePart[];
  /** Slugs that could not be resolved to a producer; the caller should block. */
  unresolved: string[];
};

export type CaretToken = {
  /** Index of the opening `${{`. */
  start: number;
  /** Caret index (replacement end). */
  end: number;
  /** The partial key being typed (after an optional `slug.`). */
  query: string;
  /** The owner slug typed before the `.`, or null when none yet. */
  ownerSlug: string | null;
};

/** A pure-literal value: a single text part. */
export function literalParts(value: string): ValuePart[] {
  return [{ kind: "text", value }];
}

export function isPureLiteral(parts: ValuePart[]): boolean {
  return parts.every((part) => part.kind === "text");
}

/**
 * Scan a display/input string for `${{ [slug.]KEY }}` references, returning the
 * owner slug (null for a self ref) and key of each. Lineage-free — used for
 * client-side impact analysis where producers are matched by their current slug
 * (delete/rename "what references this"). Escaped `$${{` tokens are ignored.
 */
export function extractDisplayRefs(
  text: string,
): { ownerSlug: string | null; key: string }[] {
  const refs: { ownerSlug: string | null; key: string }[] = [];
  TOKEN_GLOBAL.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = TOKEN_GLOBAL.exec(text)) !== null) {
    // A `$` immediately before the token means it was escaped (`$${{`).
    if (match.index >= 1 && text[match.index - 1] === "$") continue;
    refs.push({ ownerSlug: match[1] ?? null, key: match[2] ?? "" });
  }
  return refs;
}

/** All `ref` parts in order. */
export function extractRefs(parts: ValuePart[]): ParsedRef[] {
  return parts.flatMap((part) =>
    part.kind === "ref" ? [{ owner: part.owner, key: part.key }] : [],
  );
}

/**
 * The plain literal string for a value, or `null` if it contains any ref (a
 * templated value cannot be copied/exported as a literal).
 */
export function partsToLiteralString(parts: ValuePart[]): string | null {
  if (!isPureLiteral(parts)) return null;
  return parts.map((part) => (part.kind === "text" ? part.value : "")).join("");
}

function escapeLiteral(value: string): string {
  // Escape any literal `${{` so it round-trips back to a text part. A function
  // replacer avoids `$$` being treated as a special replacement pattern.
  return value.replaceAll("${{", () => "$${{");
}

/** Slug rendered for a ref whose owning producer no longer exists in the env. */
export const DELETED_OWNER_SENTINEL = "<deleted>";

function renderRef(
  owner: ValuePartRefOwner,
  key: string,
  lookupSlug: LookupSlug,
): string {
  if (owner.scope === "self") {
    return `\${{ ${key} }}`;
  }
  const slug = lookupSlug(owner.lineageId);
  return `\${{ ${slug ?? DELETED_OWNER_SENTINEL}.${key} }}`;
}

/** Whether a rendered display value references a producer that was deleted. */
export function referencesDeletedOwner(displayValue: string): boolean {
  return displayValue.includes(`\${{ ${DELETED_OWNER_SENTINEL}.`);
}

/** Render canonical parts into the `${{ slug.KEY }}` display string. */
export function partsToDisplay(
  parts: ValuePart[],
  lookupSlug: LookupSlug,
): string {
  return parts
    .map((part) =>
      part.kind === "text"
        ? escapeLiteral(part.value)
        : renderRef(part.owner, part.key, lookupSlug),
    )
    .join("");
}

/**
 * Parse a display/input string into canonical parts, resolving owner slugs to
 * lineage ids. Unresolvable slugs are kept as literal text and reported in
 * `unresolved` so the caller can reject the save.
 */
export function parseDisplayToParts(
  text: string,
  lookupLineage: LookupLineage,
): ParseDisplayResult {
  const parts: ValuePart[] = [];
  const unresolved: string[] = [];
  let pending = "";
  let i = 0;

  const flush = () => {
    if (pending) {
      parts.push({ kind: "text", value: pending });
      pending = "";
    }
  };

  while (i < text.length) {
    // Escaped literal `$${{` -> `${{`.
    if (text.startsWith("$${{", i)) {
      pending += "${{";
      i += 4;
      continue;
    }
    if (text.startsWith("${{", i)) {
      const match = TOKEN_AT_START.exec(text.slice(i));
      if (match) {
        const [raw, slug, key] = match;
        if (!key) {
          pending += raw;
          i += raw.length;
          continue;
        }
        if (slug === undefined) {
          flush();
          parts.push({ kind: "ref", owner: { scope: "self" }, key });
        } else {
          const resolved = lookupLineage(slug);
          if (resolved) {
            flush();
            parts.push({
              kind: "ref",
              owner: { scope: resolved.scope, lineageId: resolved.lineageId },
              key,
            });
          } else {
            // Keep the raw token as literal text and flag it.
            unresolved.push(slug);
            pending += raw;
          }
        }
        i += raw.length;
        continue;
      }
      // Malformed `${{` — treat as literal text.
      pending += "${{";
      i += 3;
      continue;
    }
    pending += text[i];
    i += 1;
  }

  flush();
  return { parts, unresolved };
}

/**
 * If the caret sits inside an unclosed `${{ ... ` token, describe it so the
 * autocomplete can filter and replace it. Returns null otherwise. Multi-line
 * aware (works inside a textarea).
 */
export function caretToken(text: string, caret: number): CaretToken | null {
  const open = text.lastIndexOf("${{", caret);
  if (open === -1) return null;
  // Escaped `$${{` is not an active token.
  if (open >= 1 && text[open - 1] === "$") return null;

  const inner = text.slice(open + 3, caret);
  // A closed token (`}}` before the caret) means we're past it.
  if (inner.includes("}}")) return null;

  const body = inner.replace(/^\s+/, "");
  const dot = body.indexOf(".");
  let ownerSlug: string | null = null;
  let query: string;
  if (dot === -1) {
    query = body;
  } else {
    ownerSlug = body.slice(0, dot);
    query = body.slice(dot + 1);
    if (!new RegExp(`^${SLUG_SRC}$`).test(ownerSlug)) return null;
  }
  // Reject anything that can't be part of a key prefix (e.g. spaces, `}`).
  if (query !== "" && !/^[A-Za-z0-9_]*$/.test(query)) return null;

  return { start: open, end: caret, query, ownerSlug };
}

/** Build the display token a picked autocomplete item should insert. */
export function buildRefToken(input: {
  ownerSlug: string | null;
  key: string;
}): string {
  return input.ownerSlug
    ? `\${{ ${input.ownerSlug}.${input.key} }}`
    : `\${{ ${input.key} }}`;
}
