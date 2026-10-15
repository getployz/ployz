import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { createResourceServerChallenge } from "@better-auth/oauth-provider";
import { type CallToolResult, createMcpHandler, fromJsonSchema, McpServer } from "@modelcontextprotocol/server";
import { APIError } from "better-auth/api";
import { Effect, Option, Schema } from "effect";
import { AGENT_TOOLS, type AgentCaller, type AgentResult, type AgentTool, runAgentCall } from "#/modules/agent/agent-runner.server";
import { CATALOG } from "#/modules/agent/agent-tools";
import { asRecord, asString, projectJsonObject, projectJsonValue } from "#/lib/json";
import type { JsonObject } from "#/db/tables";
import { grantClaimsOf, oauthCaller } from "#/modules/identity/caller.server";
import { REQUEST_KEY_HEADER, requestKeyOf } from "#/modules/operations/keyed.server";
import { MCP_SCOPES, mcpEndpoints } from "#/server/auth-mcp.server";
import { Auth } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";

const CORS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET, POST, DELETE, OPTIONS",
  "Access-Control-Allow-Headers": `Authorization, Content-Type, Mcp-Protocol-Version, Mcp-Method, Mcp-Name, Last-Event-ID, ${REQUEST_KEY_HEADER}, DPoP`,
  "Access-Control-Expose-Headers": `WWW-Authenticate, Mcp-Session-Id, ${REQUEST_KEY_HEADER}`,
  "Access-Control-Max-Age": "86400",
};

/** The authorization server's endpoints a browser-based agent calls cross-origin. */
const CORS_AUTH_PATHS = new Set(["/api/auth/oauth2/token", "/api/auth/oauth2/register", "/api/auth/jwks"]);

const AUTHORIZATION_SERVER = "/.well-known/oauth-authorization-server";

const notFound = () => new Response(null, { status: 404 });
const preflight = () => new Response(null, { status: 204, headers: CORS });

const withHeaders = (response: Response, headers: Record<string, string>) => {
  const merged = new Headers(response.headers);
  for (const [name, value] of Object.entries(headers)) merged.set(name, value);
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers: merged });
};

/** A grant the token names but Cloud no longer honors, answered with the challenge a missing token gets. */
const challenge = (resource: string, message: string) => {
  const error = createResourceServerChallenge(new APIError("UNAUTHORIZED", { message }), resource, { challengeScopes: [MCP_SCOPES[0]] });
  const headers = new Headers(error?.headers);
  headers.set("Content-Type", "application/json");
  return new Response(JSON.stringify({ jsonrpc: "2.0", error: { code: -32000, message }, id: null }), { status: error?.statusCode ?? 401, headers });
};

const APPROVAL = {
  type: "string",
  description: "The ID of an approval a person granted. Send it when calling this tool again after it answered approval_required.",
};

const toolSchema = (tool: AgentTool): JsonObject => {
  if (!tool.gated) return tool.inputSchema;
  const properties = asRecord(tool.inputSchema["properties"]) ?? {};
  return { ...tool.inputSchema, properties: { ...properties, approval: APPROVAL } };
};

type Run = <A, E>(effect: Effect.Effect<A, E, RemoteServices>) => Promise<A>;
type RemoteServices = Effect.Services<ReturnType<typeof runAgentCall>> | Effect.Services<ReturnType<typeof oauthCaller>>;

const text = (body: JsonObject, isError: boolean): CallToolResult =>
  ({ isError, content: [{ type: "text", text: JSON.stringify(body) }], structuredContent: body });

const resultOf = (result: AgentResult, key: string, link: () => string): CallToolResult => {
  if ("asking" in result) {
    const { approvalId, refusal } = result.asking;
    const url = link();
    return {
      isError: true,
      content: [{
        type: "text",
        text: `${refusal.message} A person must approve it: open ${url}, approve ${approvalId} under Waiting on you, then call this tool again with approval set to ${approvalId}.`,
      }],
      structuredContent: { ok: false, approval_required: { approval_id: approvalId, message: refusal.message, url }, request_key: key },
    };
  }
  const { outcome } = result;
  return text(projectJsonObject({ ...outcome, request_key: key }) ?? { ok: outcome.ok, request_key: key }, !outcome.ok);
};

/** One MCP server for one request, acting as `caller`; every request key it uses lands in `keys`. */
const serverFor = (caller: AgentCaller, request: Request, run: Run, appUrl: URL, keys: Array<string>) => {
  const server = new McpServer({ name: "ployz", version: "1.0.0" }, { instructions: CATALOG.instructions });
  const headerKey = requestKeyOf(request.headers.get(REQUEST_KEY_HEADER));
  const link = () => `${new URL(`/cloud/${caller.organization.slug}`, appUrl).href}?chat=${randomUUID()}`;
  for (const tool of AGENT_TOOLS) {
    server.registerTool(tool.name, {
      description: tool.description,
      inputSchema: fromJsonSchema<JsonObject>(toolSchema(tool)),
      annotations: { readOnlyHint: tool.readOnly, destructiveHint: !tool.readOnly },
    }, async (args) => {
      const { approval, ...input } = args;
      const sentKey = asString(input["request_key"]);
      const given = sentKey === null ? headerKey : requestKeyOf(sentKey);
      if (!given.ok) return text({ ok: false, refusal: given.refusal }, true);
      const key = given.key ?? randomUUID();
      keys.push(key);
      const sent = tool.keyed ? { ...input, request_key: key } : input;
      const result = await run(runAgentCall(caller, {
        tool: tool.name,
        input: projectJsonValue(sent) ?? null,
        key,
        approvalId: asString(approval),
        page: { at: "organization" },
      }));
      return resultOf(result, key, link);
    });
  }
  return server;
};

/**
 * `/mcp`, the remote agent endpoint on a hosted Cloud: a stateless streamable HTTP MCP server behind the OAuth grant a
 * member made on /connect. Each request verifies its token, acts as the grant's member in the grant's Organization,
 * and answers from a fresh server, so any instance serves any request.
 */
export const serveRemoteMcp = Effect.fn("RemoteMcp.serve")(function* (request: Request) {
  const auth = yield* Auth;
  if (auth.mcp === null) return notFound();
  if (request.method === "OPTIONS") return preflight();
  if (request.method !== "POST") return withHeaders(new Response(null, { status: 405 }), { ...CORS, Allow: "POST, OPTIONS" });
  const { app } = yield* AppConfig;
  const { resource } = mcpEndpoints(app.url);
  const run: Run = Effect.runPromiseWith(yield* Effect.context<RemoteServices>());
  const keys: Array<string> = [];
  const response = yield* auth.mcp.protect(request, async (verified, claims) => {
    const grant = grantClaimsOf(claims);
    const caller = grant._tag === "None"
      ? null
      : await run(oauthCaller(grant.value).pipe(Effect.catchTag("Unauthorized", () => Effect.succeed(null))));
    if (caller === null) return challenge(resource, "This connection is no longer authorized. Connect Ployz again.");
    const handler = createMcpHandler(() => serverFor(caller, verified, run, app.url, keys), { legacy: "stateless", responseMode: "json" });
    try {
      return await handler.fetch(verified);
    } finally {
      await handler.close();
    }
  });
  const key = keys.at(-1);
  return withHeaders(response, key === undefined ? CORS : { ...CORS, [REQUEST_KEY_HEADER]: key });
});

/** The OAuth metadata documents an MCP client discovers from `/mcp`; the bare authorization-server path names `/api/auth`'s. */
export const serveMcpMetadata = Effect.fn("RemoteMcp.metadata")(function* (request: Request) {
  const auth = yield* Auth;
  if (auth.mcp === null) return notFound();
  if (request.method === "OPTIONS") return preflight();
  if (request.method !== "GET" && request.method !== "HEAD") return new Response(null, { status: 405, headers: { ...CORS, Allow: "GET, HEAD, OPTIONS" } });
  const url = new URL(request.url);
  if (url.pathname === AUTHORIZATION_SERVER) url.pathname = `${AUTHORIZATION_SERVER}/api/auth`;
  return withHeaders(yield* auth.handler(new Request(url, request)), CORS);
});

const REGISTER = "/api/auth/oauth2/register";

const Registration = Schema.Struct({
  application_type: Schema.optional(Schema.String),
  redirect_uris: Schema.optional(Schema.Array(Schema.String)),
});
const decodeRegistration = Schema.decodeUnknownOption(Registration);

const isNativeRedirect = (redirect: string) => {
  if (!URL.canParse(redirect)) return false;
  const url = new URL(redirect);
  if (url.protocol !== "http:" && url.protocol !== "https:") return true;
  return url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "[::1]";
};

/**
 * Agents written before SEP-837 register a loopback redirect without `application_type`, and better-auth then judges
 * them as web clients and refuses the redirect. Infer the type from the redirects the way current MCP SDKs do.
 */
const withApplicationType = async (request: Request) => {
  const body = await request.clone().json().catch(() => null);
  const metadata = decodeRegistration(body, { onExcessProperty: "preserve" });
  if (Option.isNone(metadata) || metadata.value.application_type !== undefined) return request;
  const applicationType = (metadata.value.redirect_uris ?? []).some(isNativeRedirect) ? "native" : "web";
  const headers = new Headers(request.headers);
  headers.delete("Content-Length");
  // From parts: the server's own Request class has no undici state for `new Request(request)` to copy.
  return new Request(request.url, {
    method: request.method, headers, signal: request.signal,
    body: JSON.stringify({ ...metadata.value, application_type: applicationType }),
  });
};

/** `/api/auth/*`; on a hosted Cloud, the token, registration and key endpoints also answer browser-based agents. */
export const serveAuth = Effect.fn("RemoteMcp.auth")(function* (request: Request) {
  const auth = yield* Auth;
  const pathname = new URL(request.url).pathname;
  const cors = auth.mcp !== null && CORS_AUTH_PATHS.has(pathname);
  if (request.method === "OPTIONS") return cors ? preflight() : notFound();
  const registering = auth.mcp !== null && request.method === "POST" && pathname === REGISTER;
  const sent = registering ? yield* Effect.promise(() => withApplicationType(request)) : request;
  const response = yield* auth.handler(sent);
  return cors ? withHeaders(response, CORS) : response;
});
