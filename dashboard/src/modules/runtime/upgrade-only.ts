/**
 * A Server that runs Ployz 0.3 enforces no Device approval, so Cloud sends it only an upgrade, reads, and the upkeep of
 * Cloud's own connection to it. Every call of a session is listed here with the Servers it changes; a change that reaches
 * a Server whose daemon lacks `ployz.machine.enforced.v1` fails with `ServerLacks` before it is sent.
 */
import type { MachineTarget, RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { requirements, runsOlderPloyz, ServerLacks } from "#/modules/runtime/daemon-compat";
import { PloyzProviderError, type PloyzSdkError, type PloyzSession } from "#/modules/runtime/ployz.server";

/**
 * What a call reaches: a read, an upgrade, Cloud's own connection (its Management Client, push target and the
 * certificates it issued), every Server of the Cluster, or the Servers its arguments name.
 */
type Reach<A extends readonly unknown[]> = "read" | "upgrade" | "upkeep" | "every server" | ((...args: A) => readonly MachineTarget[]);

type Reaches = { readonly [K in keyof PloyzSession]: PloyzSession[K] extends (...args: infer A) => unknown ? Reach<A> : never };

export const REACHES = {
  logs: "read",
  logHistory: "read",
  metricHistory: "read",
  inspect: "read",
  inspectMachine: "read",
  cluster: "read",
  observeEnrollment: "read",
  dataLossIfMachineRemoved: "read",
  dataLossIfNamespaceDestroyed: "read",
  inspectContainer: "read",
  agentAsk: "read",
  observeService: "read",
  watch: "read",
  watchFirstFrame: "read",
  requestMachineUpgrade: "upgrade",
  setManagementClient: "upkeep",
  clearManagementClient: "upkeep",
  setPushTarget: "upkeep",
  publishCertificateMaterial: "upkeep",
  setClusterName: "every server",
  register: "every server",
  removeMachine: "every server",
  removeMachineMembership: "every server",
  drainMachine: "every server",
  startIngress: "every server",
  setIngressDoors: "every server",
  setPrivateAccess: "every server",
  backfillSealingKey: "every server",
  destroyNamespace: "every server",
  updateMachine: (machine) => [machine],
  stopIngress: (machine) => [machine],
  activateEnforcement: (machine) => [machine],
  setRecipientKey: (machine) => [machine],
  stopContainer: (machine) => [machine],
  startContainer: (machine) => [machine],
  removeContainer: (machine) => [machine],
  copyContainerImage: (source, _container, dest) => [source, dest],
  volumeSwitch: (machine, request) => request.command === "inspect_volume_copy" ? [] : [machine],
} as const satisfies Reaches;

type FrameMachine = RuntimeWatchView["machines"][number]["machine"];

/** The first Server `targets` names, or every Server for `every server`, that the frame shows running Ployz 0.3. */
export function olderTarget(machines: readonly FrameMachine[], targets: "every server" | readonly MachineTarget[]) {
  const reached = targets === "every server" ? machines : machines.filter((machine) => targets.includes(machine.id));
  return reached.find((machine) => runsOlderPloyz({ capabilities: machine.runtime.capabilities ?? [] }));
}

const FRAME_TIMEOUT_MS = 10_000;

/**
 * `session`, refusing every change that reaches a Server running Ployz 0.3. It reads the Servers from the last frame the
 * session watched, or one it asks for. A Server the frame doesn't list is not refused: its own daemon answers.
 */
export function upgradeOnlyOnOlder(session: PloyzSession): PloyzSession {
  let latest: RuntimeWatchView | null = null;
  const seen = (frame: RuntimeWatchView) => { latest = frame; };
  async function* recordEach(frames: AsyncIterable<RuntimeWatchView>) {
    for await (const frame of frames) {
      seen(frame);
      yield frame;
    }
  }
  const frame = Effect.suspend(() => latest !== null ? Effect.succeed(latest) : session.watchFirstFrame(FRAME_TIMEOUT_MS).pipe(
    Effect.tap((read) => Effect.sync(() => seen(read))),
    Effect.mapError((cause) => new PloyzProviderError({ operation: "read the Servers' releases", cause })),
  ));
  const admit = (targets: "every server" | readonly MachineTarget[]): Effect.Effect<void, PloyzSdkError> =>
    targets !== "every server" && targets.length === 0 ? Effect.void : frame.pipe(Effect.flatMap((read) => {
      const older = olderTarget(read.machines.map(({ machine }) => machine), targets);
      return older === undefined ? Effect.void : Effect.fail(new ServerLacks({ machineId: older.id, capability: requirements.enforced.capability }));
    }));
  const gated = (name: keyof PloyzSession) => {
    const reach: Reach<never[]> = REACHES[name] as Reach<never[]>;
    const call = session[name] as (...args: never[]) => Effect.Effect<unknown, PloyzSdkError, never>;
    if (reach === "read" || reach === "upgrade" || reach === "upkeep") return call;
    return (...args: never[]) => admit(reach === "every server" ? reach : reach(...args)).pipe(Effect.andThen(Effect.suspend(() => call(...args))));
  };
  const wrapped = Object.fromEntries(Object.keys(REACHES).map((name) => [name, gated(name as keyof PloyzSession)]));
  return {
    // SAFETY: `wrapped` has every key of `REACHES`, which `satisfies Reaches` makes exactly the session's, each the
    // session's own call or that call after `admit`, which fails only with a `PloyzSdkError`.
    ...(wrapped as unknown as PloyzSession),
    watch: (options) => session.watch(options).pipe(Effect.map(recordEach)),
    watchFirstFrame: (timeoutMs) => session.watchFirstFrame(timeoutMs).pipe(Effect.tap((read) => Effect.sync(() => seen(read)))),
  };
}
