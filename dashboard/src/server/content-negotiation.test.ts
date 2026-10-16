import { expect, it } from "vitest";
import { preferredRepresentation, type Preference } from "#/server/content-negotiation";

const CASES: ReadonlyArray<readonly [string | null, Preference]> = [
  ["text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8", "html"],
  [null, "html"],
  ["*/*", "html"],
  ["text/markdown", "markdown"],
  ["text/markdown, text/html;q=0.9", "markdown"],
  ["text/markdown;q=0.5, text/html", "html"],
  ["text/markdown, text/html", "html"],
  ["text/markdown, */*;q=0.1", "markdown"],
  ["TEXT/Markdown", "markdown"],
  ["text/*", "html"],
  ["application/json", "other"],
  ["text/markdown;q=0", "other"],
  ["text/markdown;q=0, text/html", "html"],
  ["text/html;q=0, text/markdown", "markdown"],
  ["*/*;q=0", "other"],
  ["text/markdown, garbage, text/html;q=nope", "markdown"],
  ["text/html;level=1;q=0.1, text/html;q=1, text/markdown;q=0.5", "html"],
  ["text/html;level=1, text/markdown;q=0.5", "markdown"],
  ["text/markdown;charset=utf-8, text/html;q=0.5", "markdown"],
  ['text/markdown; charset="UTF-8"', "markdown"],
  ["text/markdown;q=0.9;ext=1, text/html;q=0.5", "markdown"],
];

it.each(CASES)("Accept %j prefers %s", (accept, preference) => {
  expect(preferredRepresentation(accept)).toBe(preference);
});
