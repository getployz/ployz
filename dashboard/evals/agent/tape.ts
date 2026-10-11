import { type AdapterYieldChunk, type AnyTextAdapter, type DefaultMessageMetadataByModality, EventType, type TextOptions } from "@tanstack/ai";
import { BaseTextAdapter, type StructuredOutputOptions, type StructuredOutputResult } from "@tanstack/ai/adapters";
import { Schema } from "effect";
import type { Member } from "./trial";

/**
 * A passing live trial, kept to replay with no model: what the member said, in order, and each model call's raw
 * stream. Replayed against a fresh fixture, the same calls must still pass the same graders.
 */
export const Golden = Schema.Struct({
  task: Schema.String,
  model: Schema.String,
  member: Schema.Array(Schema.String),
  calls: Schema.Array(Schema.Array(Schema.Unknown)),
});
export type Golden = typeof Golden.Type;

type Options = TextOptions<Record<string, never>>;

/** `inner`, keeping every chunk of every call in `calls`. */
export class Recorder extends BaseTextAdapter<string, Record<string, never>, readonly ["text"], DefaultMessageMetadataByModality> {
  readonly name = "recorder";
  readonly calls: Array<Array<AdapterYieldChunk>> = [];

  constructor(private readonly inner: AnyTextAdapter) {
    super(undefined, inner.model);
  }

  async *chatStream(options: Options): AsyncIterable<AdapterYieldChunk> {
    const call: Array<AdapterYieldChunk> = [];
    this.calls.push(call);
    for await (const chunk of this.inner.chatStream(options)) {
      call.push(chunk);
      yield chunk;
    }
  }

  structuredOutput(options: StructuredOutputOptions<Record<string, never>>): Promise<StructuredOutputResult<unknown>> {
    return this.inner.structuredOutput(options);
  }
}

/** A model that answers each call with the next recorded stream, under the current run's ids. */
export class Replayer extends BaseTextAdapter<string, Record<string, never>, readonly ["text"], DefaultMessageMetadataByModality> {
  readonly name = "replayer";
  private next = 0;

  constructor(private readonly golden: Golden) {
    super(undefined, golden.model);
  }

  async *chatStream(options: Options): AsyncIterable<AdapterYieldChunk> {
    const call = this.golden.calls[this.next++];
    if (call === undefined) throw new Error(`${this.golden.task}'s golden has only ${this.golden.calls.length} model calls`);
    const { runId = crypto.randomUUID(), threadId = crypto.randomUUID() } = options;
    // SAFETY: a golden's calls are chunks a Recorder kept verbatim from an adapter.
    for (const chunk of call as ReadonlyArray<AdapterYieldChunk>) {
      yield chunk.type === EventType.RUN_STARTED || chunk.type === EventType.RUN_FINISHED ? { ...chunk, runId, threadId } : chunk;
    }
  }

  structuredOutput(): Promise<StructuredOutputResult<unknown>> {
    return Promise.reject(new Error("A replayed model writes no structured output."));
  }
}

/** `member`, keeping what it says in `said`. */
export const recording = (member: Member) => {
  const said: Array<string> = [];
  const keep = <A extends string | null>(message: A) => {
    if (message !== null) said.push(message);
    return message;
  };
  return { said, member: { open: () => member.open().then(keep), reply: (heard) => member.reply(heard).then(keep) } satisfies Member };
};

/** A member that says `said` in order, then stops. */
export const replaying = (said: ReadonlyArray<string>): Member => {
  let next = 0;
  return { open: async () => said[next++] ?? "", reply: async () => said[next++] ?? null };
};
