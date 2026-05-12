import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";

const siteRoot = "https://mintlify.wiki/getployz/ployz";
const docsRoot = join(process.cwd(), "docs-site", "src", "content", "docs");
const preservedCliPage = join(docsRoot, "commands", "current.md");

const sidebar = [
  ["Get Started", ["introduction", "quickstart", "installation"]],
  [
    "Core Concepts",
    ["concepts/primitives", "concepts/cluster-model", "concepts/data-plane", "concepts/storage"],
  ],
  [
    "Operations",
    [
      "operations/machines",
      "operations/deploy",
      "operations/branch-and-promote",
      "operations/migrate",
      "operations/rollback",
    ],
  ],
  ["Configuration", ["configuration/daemon", "configuration/networking", "configuration/runtimes"]],
  ["Architecture", ["architecture/overview", "architecture/coordination", "architecture/routing"]],
  ["CLI Reference", ["commands/current", "cli/overview", "cli/machine", "cli/deploy", "cli/branch", "cli/migrate", "cli/mesh", "cli/image", "cli/build"]],
];

const labels = new Map([
  ["commands/current", "Current Commands"],
  ["introduction", "Introduction"],
  ["quickstart", "Quickstart"],
  ["installation", "Installation"],
  ["concepts/primitives", "Primitives, Not Policies"],
  ["concepts/cluster-model", "Cluster Model"],
  ["concepts/data-plane", "Data Plane"],
  ["concepts/storage", "Storage"],
  ["operations/machines", "Machines"],
  ["operations/deploy", "Deploy"],
  ["operations/branch-and-promote", "Branch and Promote"],
  ["operations/migrate", "Migrate"],
  ["operations/rollback", "Rollback"],
  ["configuration/daemon", "Daemon"],
  ["configuration/networking", "Networking"],
  ["configuration/runtimes", "Runtimes"],
  ["architecture/overview", "Overview"],
  ["architecture/coordination", "Coordination"],
  ["architecture/routing", "Routing"],
  ["cli/overview", "Overview"],
  ["cli/machine", "Machine"],
  ["cli/deploy", "Deploy"],
  ["cli/branch", "Branch"],
  ["cli/migrate", "Migrate"],
  ["cli/mesh", "Mesh"],
  ["cli/image", "Image"],
  ["cli/build", "Build"],
]);

async function main() {
  const preservedCli = await readPreservedCliPage();
  const index = await fetchMarkdown(`${siteRoot}/index.md`);
  const llms = await fetch(`${siteRoot}/llms.txt`).then((response) => response.text());
  const slugs = [...llms.matchAll(/\(https:\/\/mintlify\.wiki\/getployz\/ployz\/(.+?)\.md\)/g)]
    .map((match) => match[1])
    .filter((slug) => !slug.startsWith("api-reference/"));

  await rm(docsRoot, { recursive: true, force: true });
  await writeDoc("index", index);

  for (const slug of slugs) {
    const markdown = await fetchMarkdown(`${siteRoot}/${slug}.md`);
    await writeDoc(slug, markdown);
  }

  await mkdir(join(docsRoot, "commands"), { recursive: true });
  await writeFile(join(docsRoot, "commands", "current.md"), preservedCli);
  await writeAstroConfig();
  await writeDocsLib();
}

async function readPreservedCliPage() {
  try {
    return await readFile(preservedCliPage, "utf8");
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
    return execFileSync("git", ["show", "HEAD:docs-site/src/content/docs/commands/current.md"], {
      encoding: "utf8",
    });
  }
}

async function fetchMarkdown(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`failed to fetch ${url}: ${response.status}`);
  return response.text();
}

async function writeDoc(slug, raw) {
  const cleaned = transformMintlifyMarkdown(raw);
  const title = cleaned.match(/^#\s+(.+)$/m)?.[1]?.trim() ?? labels.get(slug) ?? slug;
  const description =
    cleaned.match(/^>\s+(.+)$/m)?.[1]?.trim() ??
    "Ployz documentation imported from the Mintlify source site.";
  const body = cleaned.replace(/^#\s+.+\n+/, "").replace(/^>\s+.+\n+/, "");
  const frontmatter = [
    "---",
    `title: ${JSON.stringify(title)}`,
    `description: ${JSON.stringify(description)}`,
    "llms:",
    `  summary: ${JSON.stringify(description)}`,
    "---",
    "",
  ].join("\n");

  const path = join(docsRoot, `${slug}.md`);
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, `${frontmatter}${body.trim()}\n`);
}

function transformMintlifyMarkdown(raw) {
  return raw
    .replace(/^> ## Documentation Index\n(?:>.*\n)+\n?/m, "")
    .replace(/```([a-zA-Z0-9_-]+)?[^\n`]*theme=\{null\}[^\n`]*\n/g, "```$1\n")
    .replace(/<CardGroup(?:\s+[^>]*)?>\n?/g, "")
    .replace(/<\/CardGroup>\n?/g, "")
    .replace(/<AccordionGroup(?:\s+[^>]*)?>\n?/g, "")
    .replace(/<\/AccordionGroup>\n?/g, "")
    .replace(/<Accordion\s+title="([^"]+)"\s*>\n?/g, "### $1\n\n")
    .replace(/<\/Accordion>\n?/g, "\n")
    .replace(/<Card\s+([^>]*)>\n?/g, (_, attrs) => {
      const title = attr(attrs, "title") ?? "Card";
      const href = attr(attrs, "href");
      return href ? `### [${title}](${href})\n\n` : `### ${title}\n\n`;
    })
    .replace(/<\/Card>\n?/g, "\n")
    .replace(/<Steps>\n?/g, "")
    .replace(/<\/Steps>\n?/g, "")
    .replace(/<Step\s+title="([^"]+)"\s*>\n?/g, "### $1\n\n")
    .replace(/<\/Step>\n?/g, "\n")
    .replace(/<Tabs>\n?/g, "")
    .replace(/<\/Tabs>\n?/g, "")
    .replace(/<Tab\s+title="([^"]+)"\s*>\n?/g, "### $1\n\n")
    .replace(/<\/Tab>\n?/g, "\n")
    .replace(/<ParamField\s+([^>]*)>\n?/g, (_, attrs) => {
      const path = attr(attrs, "path") ?? "parameter";
      const type = attr(attrs, "type");
      const required = /\srequired(?:\s|>|$)/.test(attrs) ? " required" : "";
      return `- \`${path}\`${type ? ` (\`${type}\`)` : ""}${required}:\n\n`;
    })
    .replace(/<\/ParamField>\n?/g, "\n")
    .replace(/<Warning>\n?/g, ":::caution\n")
    .replace(/<\/Warning>\n?/g, ":::\n")
    .replace(/<Note>\n?/g, ":::note\n")
    .replace(/<\/Note>\n?/g, ":::\n")
    .replace(/<Info>\n?/g, ":::note\n")
    .replace(/<\/Info>\n?/g, ":::\n")
    .replace(/<Tip>\n?/g, ":::tip\n")
    .replace(/<\/Tip>\n?/g, ":::\n")
    .replace(/^ {2}/gm, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function attr(attrs, name) {
  return attrs.match(new RegExp(`${name}="([^"]+)"`))?.[1];
}

async function writeAstroConfig() {
  const groups = sidebar
    .map(([label, pages]) => {
      const items = pages
        .map((slug) => `            { label: ${JSON.stringify(labels.get(slug) ?? slug)}, slug: ${JSON.stringify(slug)} },`)
        .join("\n");
      return `        {\n          label: ${JSON.stringify(label)},\n          items: [\n${items}\n          ],\n        }`;
    })
    .join(",\n");

  await writeFile(
    join(process.cwd(), "docs-site", "astro.config.mjs"),
    `import { defineConfig } from "astro/config";\nimport starlight from "@astrojs/starlight";\n\nexport default defineConfig({\n  integrations: [\n    starlight({\n      title: "Ployz",\n      customCss: ["./src/styles/custom.css"],\n      pagefind: false,\n      social: [\n        {\n          icon: "github",\n          label: "GitHub",\n          href: "https://github.com/getployz/ployz",\n        },\n      ],\n      sidebar: [\n${groups}\n      ],\n    }),\n  ],\n});\n`,
  );
}

async function writeDocsLib() {
  await writeFile(
    join(process.cwd(), "docs-site", "src", "lib", "docs.ts"),
    `import { getCollection } from "astro:content";\nimport { readFile } from "node:fs/promises";\nimport { join } from "node:path";\n\nconst SECTION_ORDER = new Map(\n  [\n    "index",\n    "introduction",\n    "quickstart",\n    "installation",\n    "concepts",\n    "operations",\n    "configuration",\n    "architecture",\n    "commands",\n    "cli",\n  ].map((section, index) => [section, index]),\n);\n\nexport async function docsForLlms() {\n  const docs = await getCollection("docs", ({ data }) => data.llms?.include !== false);\n  return docs.sort((left, right) => compareDocIds(left.id, right.id));\n}\n\nfunction compareDocIds(left: string, right: string) {\n  const [leftSection] = left.split("/");\n  const [rightSection] = right.split("/");\n  const leftRank = SECTION_ORDER.get(leftSection) ?? SECTION_ORDER.size;\n  const rightRank = SECTION_ORDER.get(rightSection) ?? SECTION_ORDER.size;\n\n  if (leftRank !== rightRank) return leftRank - rightRank;\n  if (left.endsWith("/overview") && !right.endsWith("/overview")) return -1;\n  if (!left.endsWith("/overview") && right.endsWith("/overview")) return 1;\n  return left.localeCompare(right);\n}\n\nexport async function markdownForDoc(id: string) {\n  const path = join(process.cwd(), "src", "content", "docs", \`\${id}.md\`);\n  const raw = await readFile(path, "utf8");\n  return stripFrontmatter(raw).trim();\n}\n\nexport function stripFrontmatter(raw: string) {\n  return raw.replace(/^---\\n[\\s\\S]*?\\n---\\n?/, "");\n}\n\nexport function plainTextResponse(body: string) {\n  return new Response(body, {\n    headers: {\n      "Content-Type": "text/plain; charset=utf-8",\n    },\n  });\n}\n`,
  );
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
