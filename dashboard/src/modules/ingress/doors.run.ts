import type { DeploymentStatus } from "@ployz/sdk";
import { type Duration, Effect, Schedule } from "effect";
import {
  type ClusterIngressObservation,
  type DoorsAnswer,
  doorsGap,
  type DoorsStage,
  type DoorsWant,
  type MovedHostname,
  observationGap,
  type RedeployScope,
  unservedMoves,
} from "#/modules/ingress/doors";

/** What a switch does to the world, one step at a time; the run supplies the Engine, the row and the redeploys. */
export type SwitchPorts<EngineError, RowError, R> = {
  readonly open: (doors: readonly string[]) => Effect.Effect<DoorsAnswer, EngineError, R>;
  readonly at: (stage: DoorsStage) => Effect.Effect<void, RowError, R>;
  /** The generated names the switch moves, as the Servers show them now, or why Cloud can't tell. */
  readonly moving: () => Effect.Effect<{ readonly moves: readonly MovedHostname[] } | { readonly failure: string }, RowError, R>;
  /** Move the Cluster Domain's source to where `want` goes. */
  readonly flip: () => Effect.Effect<void, RowError, R>;
  /** Ship the Services publishing `moves` again as they run, so each is served under the new source. Why they aren't all, or null. */
  readonly redeploy: (moves: readonly MovedHostname[]) => Effect.Effect<string | null, RowError, R>;
};

export type SwitchEnd = { readonly ok: true } | { readonly ok: false; readonly failure: string };

/** The Doors a switch to `want` asks for, in order: both beside each other first, then only the one it goes to. */
export const switchSteps = (want: DoorsWant) => [["public", "tunnel"], [want]] as const;

/**
 * Switch the Cluster to `want`. Both Doors open on every Server before anything moves, so a connector or the ports are
 * serving before the source changes; the names that move are read while both are open, and the source changes only
 * once every one is known; the Door the switch leaves closes only once every Environment serves under the new source.
 * A step that doesn't land ends the switch there, leaving both Doors open as the first step did, and a switch asked
 * again in the same direction finds each step done and carries on from it.
 */
export const runSwitch = <EngineError, RowError, R>(want: DoorsWant, ports: SwitchPorts<EngineError, RowError, R>) => Effect.gen(function* () {
  const [both, only] = switchSteps(want);
  yield* ports.at("opening");
  const opened = doorsGap(yield* ports.open(both), both);
  if (opened !== null) return { ok: false, failure: opened } satisfies SwitchEnd;
  yield* ports.at("moving");
  const seen = yield* ports.moving();
  if ("failure" in seen) return { ok: false, failure: seen.failure } satisfies SwitchEnd;
  yield* ports.flip();
  const stuck = yield* ports.redeploy(seen.moves);
  if (stuck !== null) return { ok: false, failure: stuck } satisfies SwitchEnd;
  yield* ports.at("closing");
  const closed = doorsGap(yield* ports.open(only), only);
  if (closed !== null) return { ok: false, failure: closed } satisfies SwitchEnd;
  return { ok: true } satisfies SwitchEnd;
});

/** What redeploying the moved names needs of the Store and the Servers. */
export type RedeployPorts<E, R> = {
  /** Admit a Deployment of the Environment's named Services as they run: its id, or the Store's refusal. */
  readonly deploy: (scope: RedeployScope) => Effect.Effect<{ readonly id: string } | { readonly refused: string }, E, R>;
  /** The Deployment's status once it is no longer in flight. */
  readonly ended: (id: string) => Effect.Effect<DeploymentStatus, E, R>;
  /** One look at the Servers: the hostnames they publish now. */
  readonly observe: () => Effect.Effect<ClusterIngressObservation | null, E, R>;
};

/** How often the Servers are asked whether they serve the moved names, and for how long after the Deployments ended. */
export type RedeployTiming = { readonly poll: Duration.Input; readonly limit: Duration.Input };
export const REDEPLOY_TIMING: RedeployTiming = { poll: "2 seconds", limit: "90 seconds" };

const environmentWords = (scope: RedeployScope) => `${scope.project}/${scope.environment}`;

/**
 * Redeploy each Environment in `scope` and wait until the Servers publish every name in `moves` under the new source.
 * A Deployment's own end isn't proof: one superseded before it ran changed nothing, and one applied may not have
 * shipped the Service that moved. Why the names aren't all served, or null once the Servers show them.
 */
export const redeployMoved = <E, R>(
  moves: readonly MovedHostname[], scope: readonly RedeployScope[], ports: RedeployPorts<E, R>, timing: RedeployTiming = REDEPLOY_TIMING,
) => Effect.gen(function* () {
  if (moves.length === 0) return null;
  const asked = yield* Effect.forEach(scope, (environment) => ports.deploy(environment).pipe(Effect.map((admission) => ({ environment, admission }))));
  for (const { environment, admission } of asked) {
    if ("refused" in admission) return `${environmentWords(environment)} didn't redeploy: ${admission.refused}`;
  }
  for (const { environment, admission } of asked) {
    if ("refused" in admission) continue;
    const status = yield* ports.ended(admission.id);
    if (status !== "applied" && status !== "superseded") return `${environmentWords(environment)} didn't redeploy. Check its deployments, then switch again.`;
  }
  const seen = yield* ports.observe().pipe(Effect.repeat({
    schedule: Schedule.spaced(timing.poll).pipe(Schedule.upTo({ duration: timing.limit })),
    while: (observed) => observed === null || observed.partial || unservedMoves(observed.hostnames, moves).length > 0,
  }));
  if (seen === null || seen.partial) return observationGap(seen);
  const [unserved] = unservedMoves(seen.hostnames, moves);
  if (unserved === undefined) return null;
  const owner = scope.find(({ namespace }) => namespace === unserved.namespace);
  const where = owner === undefined ? `${unserved.namespace}/${unserved.service}` : environmentWords(owner);
  return `${unserved.to} isn't served yet. Check the deployments of ${where}, then switch again.`;
});
