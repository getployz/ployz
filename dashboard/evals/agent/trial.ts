import { type AnyTextAdapter, EventType, type StreamChunk } from "@tanstack/ai";
import { Effect, Option, Schema } from "effect";
import type { JsonValue } from "#/db/tables";
import { evalFixture } from "./fixture";
import { changed, type Snapshot, snapshot } from "./snapshot";
import type { Task } from "./tasks";

/** One tool call the agent made: its input, and the JSON result it read back, empty while an approval holds it. */
export type Call = { readonly tool: string; readonly input: JsonValue; result: string };

/** One sidebar run: a member's message, or `null` for the resumption after an approval card was answered. */
export type Turn = {
  readonly user: string | null;
  readonly said: string;
  readonly calls: ReadonlyArray<Call>;
  readonly interrupted: boolean;
  readonly errors: ReadonlyArray<string>;
};

/** The member's side of a trial: the opening message, then each reply to what the agent said, `null` to stop. */
export type Member = {
  readonly open: () => Promise<string>;
  readonly reply: (said: string) => Promise<string | null>;
};

const Outcome = Schema.fromJsonString(Schema.Struct({
  ok: Schema.optional(Schema.Boolean),
  value: Schema.optional(Schema.Unknown),
  refusal: Schema.optional(Schema.Struct({ code: Schema.String, message: Schema.String })),
}));

/** What a call's result says: `ok`, with a value or a refusal. Empty while an approval holds the call. */
export const outcome = (call: Call) => Schema.decodeUnknownOption(Outcome)(call.result).pipe(Option.getOrElse((): typeof Outcome.Type => ({})));

export type Evidence = { readonly before: Snapshot; readonly after: Snapshot; readonly transcript: ReadonlyArray<Turn> };

export type TrialResult = Evidence & { readonly failures: ReadonlyArray<string> };

export const MAX_TURNS = 20;

/** What `task` checks, plus every change it did not expect. */
export const grade = (task: Task, evidence: Evidence) => [
  ...task.check(evidence),
  ...changed(evidence.before, evidence.after).filter((key) => !task.allowed(key)).map((key) => `${key} changed`),
];

/**
 * `agent` serving `member` on a fresh fixture, `task`'s scripted policy answering each approval card, for at most
 * `MAX_TURNS` runs. A policy that never answers ends the trial at the first card.
 */
export const runTrial = Effect.fn("Eval.trial")(function* (task: Task, agent: AnyTextAdapter, member: Member) {
  const fixture = yield* evalFixture();
  if (task.setup !== undefined) yield* task.setup(fixture.write);
  const before = yield* fixture.provided(snapshot(fixture.store));
  const conversation = fixture.conversation(agent, `trial-${task.id}`);
  const transcript: Turn[] = [];
  const calls = new Map<string, Call>();
  const record = (user: string | null) => (chunks: ReadonlyArray<StreamChunk>) => {
    const turn = read(user, chunks, calls);
    transcript.push(turn);
    return turn;
  };
  const opening = yield* Effect.promise(member.open);
  let message: string | null = opening;
  while (message !== null && transcript.length < MAX_TURNS) {
    let turn: Turn = yield* conversation.say(message).pipe(Effect.map(record(message)));
    let said: string = turn.said;
    while (turn.interrupted && task.policy !== "never" && transcript.length < MAX_TURNS) {
      turn = yield* conversation.answer(task.policy === "approve" ? { approve: true } : { deny: task.policy.deny }).pipe(Effect.map(record(null)));
      said = `${said}\n${turn.said}`.trim();
    }
    if (turn.interrupted) break;
    const reply = said;
    message = task.repeat === true && transcript.length === 1 ? opening : yield* Effect.promise(() => member.reply(reply));
  }
  const after = yield* fixture.provided(snapshot(fixture.store));
  const evidence: Evidence = { before, after, transcript };
  return { ...evidence, failures: grade(task, evidence) } satisfies TrialResult;
});

/** One run's stream as a turn. A held call's result lands in its own `Call` when a later run resumes it. */
const read = (user: string | null, chunks: ReadonlyArray<StreamChunk>, calls: Map<string, Call>): Turn => {
  const made: Call[] = [];
  const names = new Map<string, string>();
  for (const chunk of chunks) {
    if (chunk.type === EventType.TOOL_CALL_START) names.set(chunk.toolCallId, chunk.toolCallName);
    if (chunk.type === EventType.TOOL_CALL_END) {
      // SAFETY: the sidebar's tools take JSON objects, which the adapter parsed from the model's arguments.
      const call: Call = { tool: names.get(chunk.toolCallId) ?? "", input: (chunk.input ?? null) as JsonValue, result: "" };
      calls.set(chunk.toolCallId, call);
      made.push(call);
    }
    if (chunk.type === EventType.TOOL_CALL_RESULT) {
      const call = calls.get(chunk.toolCallId);
      if (call !== undefined) call.result = Schema.decodeUnknownOption(Schema.String)(chunk.content).pipe(Option.getOrElse(() => JSON.stringify(chunk.content)));
    }
  }
  return {
    user,
    said: chunks.flatMap((chunk) => chunk.type === EventType.TEXT_MESSAGE_CONTENT ? [chunk.delta] : []).join(""),
    calls: made,
    interrupted: chunks.some((chunk) => chunk.type === EventType.RUN_FINISHED && chunk.outcome?.type === "interrupt"),
    errors: chunks.flatMap((chunk) => chunk.type === EventType.RUN_ERROR ? [chunk.message] : []),
  };
};
