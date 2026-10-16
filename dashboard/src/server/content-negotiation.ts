/**
 * Which representation a request's Accept header asks for. The marketing site serves Markdown copies of its pages
 * (llms.txt points agents at them); the app's own pages are HTML, so a Markdown-only client gets a Markdown answer
 * that says so instead of TanStack Start's JSON 406.
 */
export type Preference = "html" | "markdown" | "other";

type MediaRange = { readonly type: string; readonly subtype: string; readonly q: number };

function parseAccept(accept: string): ReadonlyArray<MediaRange> {
  return accept.split(",").flatMap((part) => {
    const [range = "", ...params] = part.split(";").map((piece) => piece.trim().toLowerCase());
    const [type, subtype] = range.split("/");
    if (!type || !subtype) return [];
    const qParam = params.find((param) => param.startsWith("q="));
    const q = qParam === undefined ? 1 : Number(qParam.slice(2));
    return Number.isFinite(q) ? [{ type, subtype, q: Math.min(Math.max(q, 0), 1) }] : [];
  });
}

/**
 * RFC 9110 12.5.1: the most specific matching range sets the quality. Media parameters are ignored and the highest q
 * at that level wins, so a header the parser reads loosely (text/html;level=1;q=0.1, text/html) leans to HTML, which
 * is what every page served before Markdown existed.
 */
function quality(ranges: ReadonlyArray<MediaRange>, type: string, subtype: string): number {
  const levels = [
    ranges.filter((r) => r.type === type && r.subtype === subtype),
    ranges.filter((r) => r.type === type && r.subtype === "*"),
    ranges.filter((r) => r.type === "*" && r.subtype === "*"),
  ];
  const matched = levels.find((level) => level.length > 0);
  return matched ? Math.max(...matched.map((r) => r.q)) : 0;
}

/** HTML wins ties, so a browser's Accept (or none at all) always gets the page. */
export function preferredRepresentation(accept: string | null): Preference {
  const ranges = parseAccept(accept ?? "*/*");
  const html = quality(ranges, "text", "html");
  const markdown = quality(ranges, "text", "markdown");
  if (markdown > html) return "markdown";
  return html > 0 ? "html" : "other";
}

export const requestPreference = (request: Request) => preferredRepresentation(request.headers.get("accept"));

const ORIGIN = "https://ployz.dev";

function markdownResponse(status: number, body: string): Response {
  return new Response(body, {
    status,
    headers: { "content-type": "text/markdown; charset=utf-8", vary: "Accept" },
  });
}

export const markdownNotAcceptable = () =>
  markdownResponse(
    406,
    `# No Markdown version\n\nThis page has no Markdown version. Ask for \`text/html\`, or start from ${ORIGIN}/llms.txt for the pages that have one.\n`,
  );

export const markdownNotFound = () =>
  markdownResponse(
    404,
    [
      "# Not found",
      "",
      "Nothing lives at this path. Try:",
      "",
      `- ${ORIGIN}/llms.txt: an index of the docs for agents`,
      `- ${ORIGIN}/docs.md: the docs`,
      `- ${ORIGIN}/sitemap.xml: every public page`,
      `- ${ORIGIN}/agents.md: how agents use Ployz`,
      "",
    ].join("\n"),
  );

export const authSummary = () =>
  markdownResponse(
    200,
    [
      "# Sign in to Ployz Cloud",
      "",
      `Sign-in page for Ployz Cloud. Docs are public at ${ORIGIN}/docs.md. Agents should use the CLI (\`ployz login\`) or the MCP server.`,
      "",
      `- MCP server: ${ORIGIN}/mcp (OAuth)`,
      `- Connect an agent: ${ORIGIN}/docs/account/connect-an-agent.md`,
      "",
    ].join("\n"),
  );

/**
 * TanStack Start answers any page request without text/html or *\/* in Accept with a JSON 406. A client that asked for
 * Markdown gets the same refusal in Markdown. Every rendered page varies by Accept now, so caches must key on it.
 */
export function negotiatePageResponse(request: Request, response: Response): Response {
  if (response.status === 406 && requestPreference(request) === "markdown") return markdownNotAcceptable();
  if (response.headers.get("content-type")?.startsWith("text/html")) appendVaryAccept(response.headers);
  return response;
}

function appendVaryAccept(headers: Headers) {
  const vary = headers.get("vary");
  if (vary?.split(",").some((name) => ["accept", "*"].includes(name.trim().toLowerCase()))) return;
  headers.append("vary", "Accept");
}
