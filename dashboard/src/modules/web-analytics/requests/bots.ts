import { list } from "isbot";
import type { Params } from "./statement";

type Trie = { readonly next: Map<string, Trie>; end: boolean };

const trieOf = (words: ReadonlyArray<string>) => {
  const root: Trie = { next: new Map(), end: false };
  for (const word of words) {
    let node = root;
    for (const char of word) {
      const child = node.next.get(char) ?? { next: new Map(), end: false };
      node.next.set(char, child);
      node = child;
    }
    node.end = true;
  }
  return root;
};

const literal = (char: string) => char.replace(/[^\w ]/g, "\\$&");

/** Reads the text after a match: it starts with none of the words. */
const notFollowedBy = (words: ReadonlyArray<string>) => {
  const after = (node: Trie): string => {
    const chars = [...node.next.keys()].map(literal).join("");
    const deeper = [...node.next].filter(([, child]) => !child.end).map(([char, child]) => `${literal(char)}${after(child)}`);
    return `(?:$|[^${chars}]${deeper.map((branch) => `|${branch}`).join("")})`;
  };
  return after(trieOf(words));
};

/** Reads the text before a match: it ends with none of the words. */
const notPrecededBy = (words: ReadonlyArray<string>) => {
  const before = (node: Trie): string => {
    const chars = [...node.next.keys()].map(literal).join("");
    const deeper = [...node.next].filter(([, child]) => !child.end).map(([char, child]) => `${before(child)}${literal(char)}`);
    return `(?:^|[^${chars}]${deeper.map((branch) => `|${branch}`).join("")})`;
  };
  return before(trieOf(words.map((word) => [...word].reverse().join(""))));
};

/**
 * isbot's lookaround alternatives, said in the RE2 ClickHouse's `match` speaks, which has no lookaround. Each reads the
 * same text, so the SQL and isbot call the same agents bots.
 */
const WITHOUT_LOOKAROUND = new Map([
  ["(?:^|[^g])news(?!sapphire)", `(?:^|[^g])news${notFollowedBy(["sapphire"])}`],
  ["(?<! channel/|google/)google(?!(?:wv|app|/google| pixel))", `${notPrecededBy([" channel/", "google/"])}google${notFollowedBy(["wv", "app", "/google", " pixel"])}`],
  ["(?<! cu)bots?(?:\\b|_)", `${notPrecededBy([" cu"])}bots?(?:\\b|_)`],
  ["(?<!cam)scan", `${notPrecededBy(["cam"])}scan`],
  ["(?<!lib)http", `${notPrecededBy(["lib"])}http`],
  ["java(?!;)", `java${notFollowedBy([";"])}`],
]);

const alternative = (source: string) => {
  const swapped = WITHOUT_LOOKAROUND.get(source);
  if (swapped !== undefined) return swapped;
  if (/\(\?[<=!]/.test(source)) throw new Error(`isbot has a lookaround pattern the bot filter cannot say in RE2: ${source}`);
  return source;
};

/** Every crawler and program isbot knows, as one case-insensitive RE2 pattern of constant size. */
export const BOT_PATTERN = `(?i)${list.map(alternative).join("|")}`;

/** Keeps the rows isbot takes for a person. The pattern is one constant-size value, so no number of user agents can outgrow a statement. */
export const notBot = (params: Params) => `NOT match(Ua, ${params.string(BOT_PATTERN)})`;
