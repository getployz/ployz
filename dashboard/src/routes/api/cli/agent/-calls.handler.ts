import "@tanstack/react-start/server-only";
import { Effect, Option, Schema } from "effect";
import { ERROR_TYPES, FEEDBACK_TYPES, recordAgentCall, reporterOf, SENTIMENTS, sessionIdOf, type WhatHappened } from "#/modules/analytics/agent-calls.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { Validation } from "#/server/public-error";

const Text = (max: number) => Schema.String.check(Schema.isMaxLength(max));
// Long enough that redaction sees all of an intent before the record cuts it; no one can fill a queue with more.
const LONG = 16_384;

const Client = Schema.Struct({ name: Schema.NullOr(Text(200)), version: Schema.NullOr(Text(200)) });
const Failure = Schema.Struct({ type: Schema.Literals(ERROR_TYPES), message: Text(LONG) });
const Feedback = Schema.Struct({
  type: Schema.Literals(FEEDBACK_TYPES),
  sentiment: Schema.NullOr(Schema.Literals(SENTIMENTS)),
  message: Text(LONG),
});

/** One agent call as `ployz mcp` and the `ployz` CLI report it, field for field the shape the Engine sends. */
const Report = Schema.Struct({
  surface: Schema.Literals(["local", "cli"]),
  kind: Schema.Literals(["call", "unknownTool", "initialize", "toolsList", "feedback"]),
  tool: Text(200),
  intent: Schema.NullOr(Text(LONG)),
  // The CLI sends 32 hex characters; `ployz mcp` a UUID, hyphens kept.
  session: Schema.String.check(Schema.isPattern(/^ses_[0-9a-f-]{32,36}$/)),
  startedAt: Text(40),
  durationMs: Schema.Number.check(Schema.isFinite(), Schema.isBetween({ minimum: 0, maximum: 86_400_000 })),
  error: Schema.NullOr(Failure),
  client: Client,
  serverVersion: Schema.NullOr(Text(100)),
  tools: Schema.NullOr(Schema.Array(Text(200)).check(Schema.isMaxLength(500))),
  feedback: Schema.NullOr(Feedback),
});
type Report = typeof Report.Type;

/** What the report says happened, when its fields fit its kind: a tools list names its tools, a feedback its text. */
const happened = (report: Report): WhatHappened | null => {
  switch (report.kind) {
    case "call":
      return { kind: "call", tool: report.tool, intent: report.intent, error: report.error };
    case "unknownTool":
      return { kind: "unknownTool", tool: report.tool };
    case "initialize":
      return { kind: "initialize" };
    case "toolsList":
      return report.tools === null ? null : { kind: "toolsList", tools: report.tools };
    case "feedback":
      return report.feedback === null ? null : { kind: "feedback", feedback: report.feedback };
  }
};

/**
 * `POST /api/cli/agent/calls`: `ployz mcp` and the `ployz` CLI report an agent's call as the Caller. It needs no
 * approval and answers 204 whatever PostHog does, so a report can never fail the call it describes.
 */
export const handleAgentCallReport = Effect.fn("Agent.handleCallReport")(function* (request: Request) {
  const caller = yield* resolveCaller(request.headers);
  const body: unknown = yield* Effect.tryPromise({
    try: () => request.json(),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  const report = Option.getOrNull(Schema.decodeUnknownOption(Report)(body, { onExcessProperty: "error" }));
  const startedAt = report === null ? new Date(Number.NaN) : new Date(report.startedAt);
  const what = report === null || Number.isNaN(startedAt.getTime()) ? null : happened(report);
  if (report === null || what === null) return yield* new Validation({ message: "Invalid request.", userFacing: true });
  yield* recordAgentCall({
    ...what,
    surface: report.surface,
    // Whoever sends a session id can pick it, and the CLI's names only an agent and a day, so the credential makes each one this caller's.
    session: sessionIdOf(`${caller.credential.id}:${report.session}`),
    startedAt,
    durationMs: report.durationMs,
    client: report.client,
    serverVersion: report.serverVersion,
    userAgent: request.headers.get("user-agent"),
    protocolVersion: null,
    caller: reporterOf(caller),
  });
  return new Response(null, { status: 204 });
});
