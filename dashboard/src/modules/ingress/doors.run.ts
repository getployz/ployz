import { Effect } from "effect";
import { type DoorsAnswer, doorsGap, type DoorsStage, type DoorsWant } from "#/modules/ingress/doors";

/** What a switch does to the world, one step at a time; the run supplies the Engine, the row and the redeploys. */
export type SwitchPorts<EngineError, RowError, R> = {
  readonly open: (doors: readonly string[]) => Effect.Effect<DoorsAnswer, EngineError, R>;
  readonly at: (stage: DoorsStage) => Effect.Effect<void, RowError, R>;
  /** Move the Cluster Domain's source to where `want` goes. */
  readonly flip: () => Effect.Effect<void, RowError, R>;
  /** Deploy the Environments again, so generated names are expanded under the new source. */
  readonly redeploy: () => Effect.Effect<void, RowError, R>;
};

export type SwitchEnd = { readonly ok: true } | { readonly ok: false; readonly failure: string };

/** The Doors a switch to `want` asks for, in order: both beside each other first, then only the one it goes to. */
export const switchSteps = (want: DoorsWant) => [["public", "tunnel"], [want]] as const;

/**
 * Switch the Cluster to `want`. Both Doors open on every Server before anything moves, so a connector or the ports are
 * serving before the source changes; the Door it leaves closes last. A step that doesn't land ends the switch there,
 * leaving both Doors open as the first step did.
 */
export const runSwitch = <EngineError, RowError, R>(want: DoorsWant, ports: SwitchPorts<EngineError, RowError, R>) => Effect.gen(function* () {
  const [both, only] = switchSteps(want);
  yield* ports.at("opening");
  const opened = doorsGap(yield* ports.open(both), both);
  if (opened !== null) return { ok: false, failure: opened } satisfies SwitchEnd;
  yield* ports.at("moving");
  yield* ports.flip();
  yield* ports.redeploy();
  yield* ports.at("closing");
  const closed = doorsGap(yield* ports.open(only), only);
  if (closed !== null) return { ok: false, failure: closed } satisfies SwitchEnd;
  return { ok: true } satisfies SwitchEnd;
});
