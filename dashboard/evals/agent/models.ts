import { ANTHROPIC_MODELS, createAnthropicChat } from "@tanstack/ai-anthropic";
import type { AnyTextAdapter } from "@tanstack/ai";
import { Config, Effect, Option, Redacted, Result, Schema } from "effect";
import pinned from "../models.json";
import { cliModel } from "./cli";

const Provider = Schema.Literals(["anthropic", "openai", "claude-cli", "codex-cli"]);
const Pinned = Schema.Struct({
  providers: Schema.Record(Schema.String, Provider),
  simulator: Schema.String,
  agents: Schema.Array(Schema.String),
});

/** The models `models.json` pins, each with its provider: the simulated member's, and the agents' under test. */
export const MODELS = Schema.decodeUnknownSync(Pinned)(pinned);

const KEYS = { anthropic: "ANTHROPIC_API_KEY", openai: "OPENAI_API_KEY" };
const isAnthropic = Schema.is(Schema.Literals(ANTHROPIC_MODELS));

/**
 * Model `name` as a plain adapter, or why it can't run here: its provider's key is unset, no adapter is installed, or
 * its CLI is missing.
 */
export const model = Effect.fn("Eval.model")(function* (name: string) {
  const provider = MODELS.providers[name];
  if (provider === undefined) return Result.fail(`${name} is not pinned in evals/models.json`);
  if (provider === "claude-cli" || provider === "codex-cli") return Result.map(cliModel(provider, name), (adapter): AnyTextAdapter => adapter);
  const key = yield* Config.option(Config.redacted(KEYS[provider])).pipe(Effect.orDie);
  if (Option.isNone(key)) return Result.fail(`${name} needs ${KEYS[provider]}`);
  if (provider === "openai" || !isAnthropic(name)) return Result.fail(`${name} has no ${provider} adapter installed`);
  return Result.succeed<AnyTextAdapter>(createAnthropicChat(name, Redacted.value(key.value)));
});
