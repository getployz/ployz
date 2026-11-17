import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import { POSTHOG_MCP_ANALYTICS_SOURCE, PostHogMCPAnalyticsProperty } from "@posthog/mcp";
import { Effect } from "effect";
import { type McpCapture, PostHog } from "#/modules/analytics/posthog.server";

/** Where an agent's call came from: hosted /mcp, the sidebar, `ployz mcp` on a person's machine, or the `ployz` CLI. */
export const AGENT_SURFACES = ["cloud", "sidebar", "local", "cli"] as const;
export type AgentSurface = (typeof AGENT_SURFACES)[number];

/** The name each surface's calls carry as `$mcp_server_name`, which MCP Analytics splits its tabs by. */
const SERVER_NAMES = {
  cloud: "ployz-cloud",
  sidebar: "ployz-sidebar",
  local: "ployz-local",
  cli: "ployz-cli",
} as const satisfies Record<AgentSurface, string>;

/** Why a call failed, in the few kinds MCP Analytics groups failures by. */
export const ERROR_TYPES = ["validation", "permission", "missing_context", "timeout", "rate_limited", "api_4xx", "api_5xx", "cancelled", "internal"] as const;
export type ErrorType = (typeof ERROR_TYPES)[number];
export type CallError = { readonly type: ErrorType; readonly message: string };

/** The kind of failure a refusal's code names. A code none of these knows is the caller's to fix, so it is a 4xx. */
export const errorTypeOf = (code: string): ErrorType => {
  switch (code) {
    case "invalid_argument":
    case "invalid":
    case "malformed_payload":
    case "unsupported":
    case "unknown_tool":
      return "validation";
    case "forbidden":
    case "permission_denied":
    case "unauthenticated":
    case "approval_denied":
      return "permission";
    case "not_found":
    case "claim_required":
    case "failed_precondition":
      return "missing_context";
    case "rate_limited":
      return "rate_limited";
    case "unavailable":
      return "api_5xx";
    case "internal":
      return "internal";
    case "cancelled":
      return "cancelled";
    default:
      return "api_4xx";
  }
};

export const FEEDBACK_TYPES = ["missing_capability", "issue", "praise", "other"] as const;
export const SENTIMENTS = ["positive", "neutral", "negative", "mixed"] as const;
export type Feedback = {
  readonly type: (typeof FEEDBACK_TYPES)[number];
  readonly sentiment: (typeof SENTIMENTS)[number] | null;
  readonly message: string;
};

/** Where a call came from, as the surface that received it knows it. */
export type CallOrigin = {
  readonly surface: AgentSurface;
  /** `ses_` and 32 hex characters: see {@link sessionIdOf}. */
  readonly session: string;
  /** The MCP client's own name and version, or for the CLI the agent it detected; null for what is not known. */
  readonly client: { readonly name: string | null; readonly version: string | null };
  readonly serverVersion: string | null;
  readonly userAgent: string | null;
  readonly protocolVersion: string | null;
};

/** What every report says of where it came from, when it ran and who made it. */
export type Origin = CallOrigin & {
  readonly startedAt: Date;
  readonly durationMs: number;
  readonly caller: { readonly userId: string; readonly organizationId: string };
};

/**
 * One thing an agent did, as every surface reports it. The kind says which of the rest it has, so a report of an
 * `initialize` that names a tool, or of a feedback without its text, cannot be built.
 */
export type AgentCallReport = Origin & WhatHappened;

/** What happened, without where, when or who: what a caller adds to the {@link Origin} it holds. */
export type WhatHappened = (
  /** A call that ran to its end. `intent` is null only for a call refused for lacking one. */
  | { readonly kind: "call"; readonly tool: string; readonly intent: string | null; readonly error: CallError | null }
  | { readonly kind: "unknownTool"; readonly tool: string }
  | { readonly kind: "initialize" }
  | { readonly kind: "toolsList"; readonly tools: ReadonlyArray<string> }
  | { readonly kind: "feedback"; readonly feedback: Feedback }
);

/**
 * `ses_` and the first 32 hex characters of the SHA-256 of `seed`, as MCP Analytics derives a session from an id the
 * transport carries. The seed is an id that names a conversation, never a credential, cookie or address.
 */
export const sessionIdOf = (seed: string): string => `ses_${createHash("sha256").update(seed).digest("hex").slice(0, 32)}`;

/** Who made a call, as every report names them. */
export const reporterOf = (caller: { readonly userId: string; readonly organization: { readonly id: string } }): Origin["caller"] => ({
  userId: caller.userId,
  organizationId: caller.organization.id,
});

/** The SDK exports a name for every event it captures, but not for this one. */
const UNKNOWN_TOOL_EVENT = "$mcp_unknown_tool";

/** A tool name the caller made up could carry anything, so an unknown one is recorded only if it looks like a tool's. */
const PLAUSIBLE_TOOL_NAME = /^[A-Za-z][A-Za-z0-9_.-]{0,39}$/;
const unrecognized = (name: string) => (PLAUSIBLE_TOOL_NAME.test(name) ? name : "[unrecognized]");

/**
 * MCP Analytics redacts emails, addresses, phone and card numbers and its own keys from what it is given, but not other
 * services' tokens, which an agent may paste into the reason it gives. These are the ones it leaves: a bearer header,
 * the well-known prefixes, a signed JWT, and any long opaque run with both letters and digits.
 */
const SECRET_PATTERNS: ReadonlyArray<RegExp> = [
  /\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{8,}/gi,
  /\b(?:sk|pk|rk|ghp|gho|ghu|ghs|ghr|glpat|npm|ployz|xox[abprs])[-_][A-Za-z0-9_-]{12,}/gi,
  /\bgithub_pat_[A-Za-z0-9_]{20,}/g,
  /\b(?:AKIA|ASIA)[A-Z0-9]{16}\b/g,
  /\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}/g,
  /\b(?=[A-Za-z0-9_-]*\d)(?=[A-Za-z0-9_-]*[A-Za-z])[A-Za-z0-9_-]{32,}\b/g,
];
/** Past this the text is cut before it is read, so that a very long one costs no more to redact than a long one. The SDK cuts at 2,048 after. */
const READ_LIMIT = 8192;
const redactSecrets = (text: string): string => SECRET_PATTERNS.reduce((kept, pattern) => kept.replace(pattern, "[redacted]"), text.slice(0, READ_LIMIT));

/**
 * Hands the report to MCP Analytics' own capture, which redacts credentials and personal data out of `intent` and the
 * error text, and then cuts each to 2,048 characters. No arguments or results are passed, so none can be sent.
 */
const send = (client: McpCapture, report: AgentCallReport) => {
  const { surface, session, startedAt, durationMs, client: agent, serverVersion, userAgent, protocolVersion, caller } = report;
  const named: ReadonlyArray<readonly [string, string | null]> = [
    [PostHogMCPAnalyticsProperty.ServerName, SERVER_NAMES[surface]],
    [PostHogMCPAnalyticsProperty.ServerVersion, serverVersion],
    [PostHogMCPAnalyticsProperty.ClientName, agent.name],
    [PostHogMCPAnalyticsProperty.ClientVersion, agent.version],
  ];
  const properties = Object.fromEntries(named.filter((entry): entry is readonly [string, string] => entry[1] !== null));
  // What MCP Analytics leaves out of an event when it is undefined.
  const common = {
    distinctId: caller.userId,
    groups: { organization: caller.organizationId },
    sessionId: session,
    timestamp: startedAt,
    protocolVersion: protocolVersion ?? undefined,
    clientUserAgent: userAgent ?? undefined,
    properties,
  };
  switch (report.kind) {
    case "call":
      return client.captureToolCall({
        ...common,
        toolName: report.tool,
        durationMs,
        isError: report.error !== null,
        intent: report.intent === null ? undefined : redactSecrets(report.intent),
        errorType: report.error?.type,
        error: report.error === null ? undefined : redactSecrets(report.error.message),
      });
    case "unknownTool":
      // MCP Analytics' own client has no capture for it, so this is the event as its docs describe it.
      return client.capture({
        distinctId: caller.userId,
        event: UNKNOWN_TOOL_EVENT,
        groups: common.groups,
        timestamp: startedAt,
        properties: {
          ...properties,
          [PostHogMCPAnalyticsProperty.Source]: POSTHOG_MCP_ANALYTICS_SOURCE,
          [PostHogMCPAnalyticsProperty.SessionId]: session,
          [PostHogMCPAnalyticsProperty.ToolName]: unrecognized(report.tool),
          [PostHogMCPAnalyticsProperty.Interface]: "mcp",
        },
      });
    case "initialize":
      return client.captureInitialize({
        ...common,
        durationMs,
        clientName: agent.name ?? undefined,
        clientVersion: agent.version ?? undefined,
      });
    case "toolsList":
      return client.captureToolsList({ ...common, durationMs, toolNames: [...report.tools] });
    case "feedback":
      return client.captureFeedback({
        ...common,
        report: {
          feedbackType: report.feedback.type,
          summary: redactSecrets(report.feedback.message),
          sentiment: report.feedback.sentiment ?? undefined,
          extras: {},
          raw: {},
        },
      });
  }
};

/**
 * The one place an agent's call becomes PostHog events. Every surface hands its report here: `runAgentCall` for hosted
 * /mcp and the sidebar, the report route for `ployz mcp` and the CLI. It never fails and never changes an answer: MCP
 * Analytics only queues, and whatever it throws is logged by the wrapper.
 */
export const recordAgentCall = (report: AgentCallReport): Effect.Effect<void> =>
  Effect.flatMap(PostHog, (posthog) => posthog.mcp((client) => send(client, report))).pipe(
    Effect.catchCause((cause) => Effect.logWarning("An agent call was not recorded.", cause)),
  );
