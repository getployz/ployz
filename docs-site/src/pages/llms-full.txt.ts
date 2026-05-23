import { docsForLlms, markdownForDoc, plainTextResponse } from "../lib/docs";

export async function GET() {
  const docs = await docsForLlms();
  const sections = await Promise.all(
    docs.map(async (doc) => {
      const markdown = await markdownForDoc(doc.id.replace(/\.mdx?$/, ""));
      return [`# ${doc.data.title}`, "", markdown].join("\n");
    }),
  );

  const header = [
    "# Ployz Open Core Documentation Bundle",
    "",
    "Generated from Markdown docs in `src/content/docs`. Do not edit this output by hand.",
  ].join("\n");

  return plainTextResponse([header, ...sections].join("\n\n---\n\n"));
}
