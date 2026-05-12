import { getCollection } from "astro:content";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

const SECTION_ORDER = new Map(
  [
    "index",
    "introduction",
    "quickstart",
    "installation",
    "concepts",
    "operations",
    "configuration",
    "architecture",
    "commands",
    "cli",
  ].map((section, index) => [section, index]),
);

export async function docsForLlms() {
  const docs = await getCollection("docs", ({ data }) => data.llms?.include !== false);
  return docs.sort((left, right) => compareDocIds(left.id, right.id));
}

function compareDocIds(left: string, right: string) {
  const [leftSection] = left.split("/");
  const [rightSection] = right.split("/");
  const leftRank = SECTION_ORDER.get(leftSection) ?? SECTION_ORDER.size;
  const rightRank = SECTION_ORDER.get(rightSection) ?? SECTION_ORDER.size;

  if (leftRank !== rightRank) return leftRank - rightRank;
  if (left.endsWith("/overview") && !right.endsWith("/overview")) return -1;
  if (!left.endsWith("/overview") && right.endsWith("/overview")) return 1;
  return left.localeCompare(right);
}

export async function markdownForDoc(id: string) {
  const path = join(process.cwd(), "src", "content", "docs", `${id}.md`);
  const raw = await readFile(path, "utf8");
  return stripFrontmatter(raw).trim();
}

export function stripFrontmatter(raw: string) {
  return raw.replace(/^---\n[\s\S]*?\n---\n?/, "");
}

export function plainTextResponse(body: string) {
  return new Response(body, {
    headers: {
      "Content-Type": "text/plain; charset=utf-8",
    },
  });
}
