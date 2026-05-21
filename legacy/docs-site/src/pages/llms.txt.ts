import { docsForLlms, plainTextResponse } from "../lib/docs";

export async function GET() {
  const docs = await docsForLlms();
  const lines = [
    "# Ployz Docs",
    "",
    "Open-core docs for `ployzctl`, `ployzd`, local development, agents, and self-hosted small clusters.",
    "Ployz Cloud dashboard, billing, team, hosted pool, and managed web UI docs are out of scope.",
    "",
    "Important command rule: north-star commands are product targets. Do not treat them as executable unless they appear in the current command surface.",
    "",
    "## Docs",
    "",
    ...docs.map((doc) => {
      const summary = doc.data.llms?.summary;
      const suffix = summary ? ` - ${summary}` : "";
      const path = doc.id === "index" ? "/" : `/${doc.id}/`;
      return `- ${path} - ${doc.data.title}${suffix}`;
    }),
    "",
    "Full generated bundle: /llms-full.txt",
  ];

  return plainTextResponse(lines.join("\n"));
}
