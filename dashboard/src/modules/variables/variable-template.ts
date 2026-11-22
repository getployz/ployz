/**
 * The `${{ }}` reference grammar, shared by the client (autocomplete, add form, display) and the server, so it must
 * not import anything `server-only`. References name a Service by its name, as the Store shows them.
 *
 *  - `${{ KEY }}`            a variable of the Service the text belongs to
 *  - `${{ name.KEY }}`       the exported `KEY` of Service `name`
 *  - `${{ <deleted name>.KEY }}` a removed Service: the Engine renders `<deleted name>`, `<deleted name#2>` when
 *    removed Services share a name, `<deleted #3f2a9c01>` when no record names it, and `<deleted>` when unknown
 *  - `$${{`                  a literal `${{`
 */

/** One piece of a value: literal text, or a reference (`owner` null is the Service the text belongs to). */
type ValuePart =
  | { kind: "text"; value: string }
  | { kind: "ref"; owner: string | null; key: string };

export const KEY_SRC = "[A-Za-z_][A-Za-z0-9_]*";
export const SLUG_SRC = "[A-Za-z0-9][A-Za-z0-9_-]*";

const DELETED_SRC = `<deleted(?: (?:${SLUG_SRC}(?:#[1-9][0-9]*)?|#[0-9a-f]{8}))?>`;

// Anchored matcher for a single `${{ [slug.]KEY }}` token at the start of a string.
const TOKEN_AT_START = new RegExp(
  `^\\$\\{\\{\\s*(?:(${SLUG_SRC}|${DELETED_SRC})\\.)?(${KEY_SRC})\\s*\\}\\}`,
);

type ParseDisplayResult = {
  parts: ValuePart[];
  /** Names `isKnown` rejected; their tokens remain literal text. */
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

const DELETED_REF = new RegExp(`(?<!\\$)\\$\\{\\{\\s*${DELETED_SRC}\\.`);

/** Whether a rendered display value references a producer that was deleted. */
export function referencesDeletedOwner(displayValue: string): boolean {
  return DELETED_REF.test(displayValue);
}

/** Render parts into the `${{ name.KEY }}` text; a literal `${{` is escaped as `$${{`. */
function partsToDisplay(parts: ValuePart[]): string {
  return parts.map((part) => {
    if (part.kind === "text") return part.value.replaceAll("${{", () => "$${{");
    return `\${{ ${part.owner === null ? "" : `${part.owner}.`}${part.key} }}`;
  }).join("");
}

/** Parse text into parts. A name `isKnown` rejects stays literal text and is reported in `unresolved`. */
export function parseDisplayToParts(
  text: string,
  isKnown: (name: string) => boolean,
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
          parts.push({ kind: "ref", owner: null, key });
        } else if (isKnown(slug)) {
          flush();
          parts.push({ kind: "ref", owner: slug, key });
        } else {
          unresolved.push(slug);
          pending += raw;
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

/**
 * `text`, a variable of Service `owner` as the Store shows it (references by Service name), with each reference to
 * variable `key` of Service `service` switched to `to`; null when it has none.
 */
export function renameReferences(text: string, owner: string, service: string, key: string, to: string): string | null {
  const { parts } = parseDisplayToParts(text, () => true);
  let renamed = false;
  const switched = parts.map((part): ValuePart => {
    if (part.kind !== "ref" || part.key !== key) return part;
    if ((part.owner ?? owner) !== service) return part;
    renamed = true;
    return { ...part, key: to };
  });
  return renamed ? partsToDisplay(switched) : null;
}
