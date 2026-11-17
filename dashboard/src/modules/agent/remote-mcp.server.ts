import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { createResourceServerChallenge } from "@better-auth/oauth-provider";
import { type CallToolResult, createMcpHandler, fromJsonSchema, type JsonSchemaValidator, type jsonSchemaValidator, McpServer } from "@modelcontextprotocol/server";
import { APIError } from "better-auth/api";
import { Effect, Option, Schema } from "effect";
import type { ToolOutcome } from "#/modules/agent/agent";
import { AGENT_TOOLS, type AgentCaller, type AgentResult, type AgentTool, runAgentCall } from "#/modules/agent/agent-runner.server";
import { CATALOG } from "#/modules/agent/agent-tools";
import { findCommand } from "#/modules/commands/commands.server";
import { asRecord, asString, projectJsonObject, projectJsonValue } from "#/lib/json";
import type { JsonObject } from "#/db/tables";
import { type CallOrigin, FEEDBACK_TYPES, recordAgentCall, reporterOf, SENTIMENTS, sessionIdOf, type WhatHappened } from "#/modules/analytics/agent-calls.server";
import { grantClaimsOf, oauthCaller } from "#/modules/identity/caller.server";
import { claimUrlFor } from "#/modules/identity/claim-required.server";
import { ORGANIZATION_TOKEN_PREFIX } from "#/modules/identity/organization-token.server";
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
  description: "The ID of an approval a human granted. Send it when calling this tool again after it answered approval_required.",
};

const BAD_APPROVAL = { code: "invalid_argument", message: "This input does not fit: approval is an approval's ID, a string.", field: "approval", expected: APPROVAL, writes: "none" };

/** Every argument reaches the tool, which refuses a bad one in the same shape as any other refusal. */
const toolValidates: jsonSchemaValidator = {
  getValidator<T>(): JsonSchemaValidator<T> {
    // SAFETY: nothing reads `data` as `T`; the handler parses the arguments and the tool validates them.
    return (input) => ({ valid: true, data: input as T, errorMessage: undefined });
  },
};

const toolSchema = (tool: AgentTool): JsonObject => {
  if (!tool.gated) return tool.inputSchema;
  const properties = asRecord(tool.inputSchema["properties"]) ?? {};
  return { ...tool.inputSchema, properties: { ...properties, approval: APPROVAL } };
};

type Run = <A, E>(effect: Effect.Effect<A, E, RemoteServices>) => Promise<A>;
type RemoteServices =
  | Effect.Services<ReturnType<typeof runAgentCall>>
  | Effect.Services<ReturnType<typeof oauthCaller>>
  | Effect.Services<ReturnType<typeof claimUrlFor>>;

/**
 * What every result's `structuredContent` holds, as `text` and `resultOf` write it; an answer's own fields stay open.
 * Hosts read it. Nothing here checks a result against it: `toolValidates` passes every one.
 */
const RESULT: JsonObject = {
  type: "object",
  properties: {
    ok: { type: "boolean", description: "Whether the call did what it was asked." },
    value: { description: "What a call that succeeded answers." },
    request_key: { type: "string", description: "The request key the call ran under: the one sent, or one made for it." },
    refusal: {
      type: "object",
      description: "Why the call was refused, with what to change or call next when the tool knows.",
      properties: {
        code: { type: "string", description: "The kind of refusal, such as not_found or invalid_argument." },
        message: { type: "string", description: "The refusal in words." },
      },
      required: ["code", "message"],
    },
    approval_required: {
      type: "object",
      description: "A human must approve the call before it runs.",
      properties: {
        approval_id: { type: "string", description: "The approval to send as approval once a person approved it." },
        message: { type: "string", description: "What waits for approval." },
        url: { type: "string", description: "Where a human approves it." },
      },
      required: ["approval_id", "message", "url"],
    },
    claim_required: {
      type: "object",
      description: "A human must claim the Organization before the call can run.",
      properties: {
        message: { type: "string", description: "Why the call needs a claimed Organization." },
        url: { type: "string", description: "Where a person claims it." },
      },
      required: ["message", "url"],
    },
  },
  required: ["ok"],
};

const text = (body: JsonObject, isError: boolean): CallToolResult =>
  ({ isError, content: [{ type: "text", text: JSON.stringify(body) }], structuredContent: body });

type Claim = { message: string; url: string };

/** A published command's retry names the approval its call carried, which stays good while the plan it approved does. */
const carrying = (tool: AgentTool, approval: string | null, result: AgentResult): AgentResult => {
  if (approval === null || findCommand(tool.name) === undefined || !("outcome" in result) || !("refusal" in result.outcome)) return result;
  const { refusal } = result.outcome;
  if ("next" in refusal || !("retry" in refusal) || refusal.retry === undefined) return result;
  const input = asRecord(refusal.retry.input);
  return input === null ? result : { outcome: { ok: false, refusal: { ...refusal, retry: { ...refusal.retry, input: { ...input, approval } } } } };
};

const resultOf = (tool: string, result: AgentResult, key: string, link: () => string, claim: Claim | null): CallToolResult => {
  if ("asking" in result) {
    const { approvalId, refusal } = result.asking;
    const url = link();
    return {
      isError: true,
      content: [{
        type: "text",
        text: `${refusal.message} A human must approve it: open ${url}, approve ${approvalId} under Waiting on you, then call this tool again with approval set to ${approvalId}.`,
      }],
      structuredContent: { ok: false, approval_required: { approval_id: approvalId, message: refusal.message, url }, request_key: key },
    };
  }
  const { outcome } = result;
  if (claim !== null) {
    return {
      isError: true,
      content: [{ type: "text", text: `${claim.message} A human must claim it first: open ${claim.url} and sign in with GitHub, then call this tool again.` }],
      structuredContent: { ok: false, claim_required: claim, request_key: key },
    };
  }
  const answer = tool === "proposal_present" ? withCard(outcome, link()) : outcome;
  return text(projectJsonObject({ ...answer, request_key: key }) ?? { ok: outcome.ok, request_key: key }, !outcome.ok);
};

/** A presented proposal waits on a person like an approval does, so its answer names the card to open. */
const withCard = (outcome: ToolOutcome, url: string): ToolOutcome => {
  const value = asRecord(outcome.ok ? outcome.value : null);
  if (!outcome.ok || value === null) return outcome;
  const message = `${asString(value["message"]) ?? "Presented."} A human must approve it: open ${url} and approve it under Waiting on you.`;
  return { ...outcome, value: { ...value, message, url } };
};

const SERVER_VERSION = "1.0.0";

const FEEDBACK_TOOL = "send_feedback";
const FEEDBACK_SCHEMA: JsonObject = {
  type: "object",
  additionalProperties: false,
  properties: {
    type: { type: "string", enum: [...FEEDBACK_TYPES], description: "`missing_capability` for something you needed and Ployz does not do. `issue` for something that went wrong. `praise` for something that worked well. `other` for the rest." },
    message: { type: "string", description: "What you needed, what went wrong or what worked, in a few sentences. Refer to people by role (\"a customer\") rather than by name. Leave out personal and identifying information." },
    sentiment: { type: "string", enum: [...SENTIMENTS], description: "How the work felt, when you want to say." },
  },
  required: ["type", "message"],
};
const FeedbackArguments = Schema.Struct({
  type: Schema.Literals(FEEDBACK_TYPES),
  message: Schema.String,
  sentiment: Schema.optional(Schema.Literals(SENTIMENTS)),
});
const decodeFeedback = Schema.decodeUnknownOption(FeedbackArguments, { onExcessProperty: "error" });

/** What an MCP request's JSON-RPC body says of the client, which the tool handlers of a stateless server never see. */
type Hello = { readonly method: string | null; readonly tool: string | null; readonly client: CallOrigin["client"]; readonly protocolVersion: string | null };

const CLIENT_INFO_META = "io.modelcontextprotocol/clientInfo";
const PROTOCOL_META = "io.modelcontextprotocol/protocolVersion";

const helloOf = async (request: Request): Promise<Hello> => {
  const body: unknown = await request.clone().json().catch(() => null);
  const message = asRecord(Array.isArray(body) ? body[0] : body);
  const params = asRecord(message?.["params"]);
  const meta = asRecord(params?.["_meta"]);
  const info = asRecord(params?.["clientInfo"] ?? meta?.[CLIENT_INFO_META]);
  return {
    method: asString(message?.["method"]),
    tool: asString(params?.["name"]),
    client: { name: asString(info?.["name"]), version: asString(info?.["version"]) },
    protocolVersion: asString(params?.["protocolVersion"] ?? meta?.[PROTOCOL_META]) ?? request.headers.get("mcp-protocol-version"),
  };
};

/**
 * Where a call came from. A client that keeps an `Mcp-Session-Id` is one session. Otherwise the grant, which names one
 * connection of one member, and the day make one: a stateless server has nothing finer.
 */
const originOf = (caller: AgentCaller, request: Request, hello: Hello): CallOrigin => ({
  surface: "cloud",
  session: sessionIdOf(request.headers.get("mcp-session-id") ?? `${caller.credential.id}:${new Date().toISOString().slice(0, 10)}`),
  client: hello.client,
  serverVersion: SERVER_VERSION,
  userAgent: request.headers.get("user-agent"),
  protocolVersion: hello.protocolVersion,
});

/** One MCP server for one request, acting as `caller`; every request key it uses lands in `keys`. */
const serverFor = (caller: AgentCaller, request: Request, origin: CallOrigin, run: Run, appUrl: URL, keys: Array<string>) => {
  const server = new McpServer({
    name: "ployz",
    title: CATALOG.title,
    version: SERVER_VERSION,
    description: CATALOG.description,
    websiteUrl: appUrl.origin,
    icons: [{ src: new URL("/assets/logo-mark.svg", appUrl).href, mimeType: "image/svg+xml", sizes: ["any"] }],
  }, { instructions: CATALOG.instructions });
  const headerKey = requestKeyOf(request.headers.get(REQUEST_KEY_HEADER));
  const link = () => `${new URL(`/cloud/${caller.organization.slug}`, appUrl).href}?chat=${randomUUID()}`;
  for (const tool of AGENT_TOOLS) {
    server.registerTool(tool.name, {
      title: tool.title,
      description: tool.description,
      inputSchema: fromJsonSchema<JsonObject>(toolSchema(tool), toolValidates),
      outputSchema: fromJsonSchema<JsonObject>(RESULT, toolValidates),
      annotations: { readOnlyHint: tool.readOnly, destructiveHint: tool.destructive, idempotentHint: tool.idempotent, openWorldHint: tool.openWorld },
    }, async (args) => {
      const fields = asRecord(args) ?? {};
      const { approval, ...rest } = fields;
      // A call this surface refuses before it can run goes through `runAgentCall` too, which records it as the failed call it is.
      const reject = async (refusal: JsonObject & { readonly message: string }) => {
        await run(runAgentCall(caller, { tool: tool.name, input: projectJsonValue(fields) ?? null, key: "", approvalId: null, page: { at: "organization" }, origin, ended: { type: "validation", message: refusal.message } }));
        return text({ ok: false, refusal }, true);
      };
      if (tool.gated && approval !== undefined && asString(approval) === null) return reject(BAD_APPROVAL);
      const input = tool.gated ? rest : fields;
      const sentKey = asString(input["request_key"]);
      const given = sentKey === null ? headerKey : requestKeyOf(sentKey);
      if (!given.ok) return reject(given.refusal);
      const key = given.key ?? randomUUID();
      keys.push(key);
      const sent = tool.keyed ? { ...input, request_key: key } : input;
      const approvalId = tool.gated ? asString(approval) : null;
      const result = carrying(tool, approvalId, await run(runAgentCall(caller, {
        tool: tool.name,
        input: projectJsonValue(sent) ?? null,
        key,
        approvalId,
        page: { at: "organization" },
        origin,
      })));
      const refused = "outcome" in result && "refusal" in result.outcome ? result.outcome.refusal : null;
      const claim = refused?.code === "claim_required" ? { message: refused.message, url: await run(claimUrlFor(caller.organization.id)) } : null;
      return resultOf(tool.name, result, key, link, claim);
    });
  }
  server.registerTool(FEEDBACK_TOOL, {
    title: "Send feedback to the Ployz team",
    description: "Tell the Ployz team what you needed and could not do, what went wrong, or what worked. Use `missing_capability` when no tool does what the work called for. It needs no `intent`.",
    inputSchema: fromJsonSchema<JsonObject>(FEEDBACK_SCHEMA, toolValidates),
    outputSchema: fromJsonSchema<JsonObject>(RESULT, toolValidates),
    annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
  }, async (args) => {
    const startedAt = new Date();
    // It needs no `intent`, and one sent anyway is ignored, as `ployz mcp` ignores it.
    const { intent: _intent, ...fields } = asRecord(args) ?? {};
    const feedback = decodeFeedback(fields);
    const record = (what: WhatHappened) =>
      run(recordAgentCall({ ...origin, ...what, startedAt, durationMs: Date.now() - startedAt.getTime(), caller: reporterOf(caller) }));
    if (Option.isNone(feedback)) {
      const message = "This input does not fit: send `type` (missing_capability, issue, praise or other), `message` and, if you like, `sentiment` (positive, neutral, negative or mixed).";
      await record({ kind: "call", tool: FEEDBACK_TOOL, intent: null, error: { type: "validation", message } });
      return text({ ok: false, refusal: { code: "invalid_argument", message } }, true);
    }
    const { type, message, sentiment } = feedback.value;
    await record({ kind: "feedback", feedback: { type, message, sentiment: sentiment ?? null } });
    return text({ ok: true, value: { message: "Thanks. The Ployz team reads this." } }, false);
  });
  return server;
};

/** Every tool `/mcp` lists. */
const LISTED_TOOLS = [...AGENT_TOOLS.map((tool) => tool.name), FEEDBACK_TOOL];

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
  if (request.headers.get("authorization")?.startsWith(`Bearer ${ORGANIZATION_TOKEN_PREFIX}`) === true) {
    return withHeaders(challenge(resource, "/mcp takes a Connect sign-in, not a Ployz token. For a token, run ployz mcp with PLOYZ_TOKEN."), CORS);
  }
  const run: Run = Effect.runPromiseWith(yield* Effect.context<RemoteServices>());
  const keys: Array<string> = [];
  const response = yield* auth.mcp.protect(request, async (verified, claims) => {
    const grant = grantClaimsOf(claims);
    const caller = grant._tag === "None"
      ? null
      : await run(oauthCaller(grant.value).pipe(Effect.catchTag("Unauthorized", () => Effect.succeed(null))));
    if (caller === null) return challenge(resource, "This connection is no longer authorized. Connect Ployz again.");
    const hello = await helloOf(verified);
    const origin = originOf(caller, verified, hello);
    const handler = createMcpHandler(() => serverFor(caller, verified, origin, run, app.url, keys), { legacy: "stateless", responseMode: "json" });
    const startedAt = new Date();
    try {
      const answer = await handler.fetch(verified);
      // A stateless server holds the handshake and the list in no tool, so they are recorded here, from the request.
      // A call to a tool the server never registered reaches no handler either.
      const unknown = hello.method === "tools/call" && hello.tool !== null && !LISTED_TOOLS.includes(hello.tool) ? hello.tool : null;
      const seen: WhatHappened | null = unknown !== null ? { kind: "unknownTool", tool: unknown }
        : !answer.ok ? null
        : hello.method === "initialize" ? { kind: "initialize" }
        : hello.method === "tools/list" ? { kind: "toolsList", tools: LISTED_TOOLS }
        : null;
      if (seen !== null) {
        await run(recordAgentCall({ ...origin, ...seen, startedAt, durationMs: Date.now() - startedAt.getTime(), caller: reporterOf(caller) }));
      }
      return answer;
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

/** ployz.dev's public key in the MCP Registry, proving Ployz owns the `dev.ployz/*` names it lists `/mcp` under. */
const REGISTRY_PROOF = "v=MCPv1; k=ed25519; p=Nrlq1rzb7NtaJi4Kc+sxSE4ual9vUlqy79z/O0trwxo=";

export const serveRegistryProof = Effect.fn("RemoteMcp.registryProof")(function* (request: Request) {
  const auth = yield* Auth;
  if (auth.mcp === null) return notFound();
  if (request.method !== "GET" && request.method !== "HEAD") return new Response(null, { status: 405, headers: { Allow: "GET, HEAD" } });
  return new Response(REGISTRY_PROOF, { headers: { "Content-Type": "text/plain; charset=utf-8" } });
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
