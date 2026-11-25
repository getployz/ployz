import type { ContainerId, DrainReport, MachineId, MachineRef, QualifiedService, RuntimeWatchView, ServiceDrain } from "@ployz/sdk";
import { listNames } from "#/lib/plural";
import { requirements, serverSupports } from "#/modules/runtime/daemon-compat";
import { REGION_LABEL } from "#/modules/runtime/runtime-watch-frame";
import { splitQualifiedService } from "#/modules/machines/server-services";
import type { VolumeRunState } from "#/modules/volume-run/volume-run-states";

type Container = RuntimeWatchView["containers"][number];

/**
 * What the frame says about moving a Service the Engine left on its drained Server for its data: it may move with its
 * one Managed volume, it stays for a reason Cloud names, or it mounts a folder or a Docker volume, which the Engine's
 * own words already name.
 */
export type DataCheck =
  | { readonly kind: "moves"; readonly dockerVolume: string; readonly holder: ContainerId }
  | { readonly kind: "stays"; readonly why: string }
  | { readonly kind: "engine" };

const mountsVolume = (container: Container, dockerVolume: string) =>
  container.resolved_spec.volumes.some(({ source }) => source.kind === "provisioned" && source.name === dockerVolume);

const serviceName = (container: Container) => container.resolved_spec.name;

/**
 * The Service's data on `machineId` may move with it when it mounts one Managed volume and nothing else that lasts, one
 * Container runs on that Volume and it is this Service's, no Container of anything else uses it on any Server, and no
 * copy of it runs on every Server: a Deploy may have made it a Global since the Engine left it here.
 */
export function dataCheck(frame: RuntimeWatchView, service: QualifiedService, machineId: string): DataCheck {
  const { namespace, name } = splitQualifiedService(service);
  const own = (container: Container) => container.namespace === namespace && serviceName(container) === name;
  const here = frame.containers.filter((container) => container.machine_id === machineId && container.kind === "service_container" && own(container));
  const [spec] = [...here].sort((a, b) => b.created_at_unix_nanos - a.created_at_unix_nanos);
  if (spec === undefined) return { kind: "stays", why: "it no longer runs here" };
  if (frame.containers.some((container) => own(container) && container.resolved_spec.mode.mode === "global")) {
    return { kind: "stays", why: "it runs on every server, and a move takes a Service with a replica count" };
  }
  const lasting = spec.resolved_spec.volumes.filter(({ source }) => source.kind !== "tmpfs");
  if (lasting.some(({ source }) => source.kind !== "provisioned")) return { kind: "engine" };
  const [one, ...more] = lasting;
  if (one === undefined || one.source.kind !== "provisioned") return { kind: "engine" };
  // A Volume's reference is its id, not the name its user gave it, so a reason counts Volumes rather than naming one.
  if (more.length > 0) return { kind: "stays", why: `it mounts ${lasting.length} volumes, and a move takes one` };
  const dockerVolume = one.source.name;
  const users = frame.containers.filter((container) => mountsVolume(container, dockerVolume));
  const others = users.filter((container) => !own(container) || container.machine_id !== machineId);
  if (others.length > 0) {
    const named = [...new Set(others.map((container) => container.machine_id === machineId ? serviceName(container) : container.display_name))];
    return { kind: "stays", why: `it shares its volume with ${listNames(named)}` };
  }
  const running = users.filter((container) => container.machine_id === machineId && container.runtime.state === "running");
  const [holder, second] = running;
  if (holder === undefined) return { kind: "stays", why: "it isn't running here, so nothing would start it elsewhere" };
  if (second !== undefined) return { kind: "stays", why: `${running.length} of its containers run on its volume, and a move takes one` };
  return { kind: "moves", dockerVolume, holder: holder.container_id };
}

/** What the drain dialog says of a Service whose data is on the Server: it may move with it, or it stays and why. */
export type DataPlan = { readonly kind: "moves" } | { readonly kind: "stays"; readonly why: string };

export const dataPlan = (check: DataCheck): DataPlan =>
  check.kind === "moves" ? { kind: "moves" } : { kind: "stays", why: check.kind === "stays" ? check.why : "it uses a folder or a Docker volume here" };

/** The Server holding a copy of `dockerVolume` beside its writer: the planner moves only onto it while it exists. */
export function copyHolder(frame: RuntimeWatchView, dockerVolume: string): MachineId | null {
  const slot = frame.volumes.find(({ id, storage }) => id.name === dockerVolume && storage.kind === "provisioned" && storage.role === "slot");
  return slot?.id.machine_id ?? null;
}

/**
 * The Servers a Volume may move to from `from`, best first: each answers, takes services, isn't draining, can start a
 * moved Service and has managed storage, and sits in a region the Service's signed spec runs in (any region when it
 * names none). The Server already holding a copy comes first, then those in `from`'s region, each group by name.
 */
export function candidateServers(frame: RuntimeWatchView, input: {
  readonly from: MachineId;
  readonly regions: readonly string[];
  readonly copy: MachineId | null;
  readonly draining: ReadonlySet<MachineId>;
}): MachineRef[] {
  const unanswered = new Set(frame.unanswered);
  const regionOf = (labels: Readonly<Record<string, string>>) => labels[REGION_LABEL] ?? null;
  const source = frame.machines.find(({ machine }) => machine.id === input.from);
  const home = source === undefined ? null : regionOf(source.machine.labels);
  const fits = frame.machines.filter(({ machine, storage }) => {
    if (machine.id === input.from || unanswered.has(machine.id) || input.draining.has(machine.id) || !machine.accepts_services) return false;
    if (serverSupports({ capabilities: machine.runtime.capabilities ?? [] }, requirements.volumeStart) !== true) return false;
    // A Server makes its Machine Pool with its first Managed volume, so one that never held one reads `ready`.
    if (storage === null || storage === undefined || storage.state === "stateless") return false;
    const region = regionOf(machine.labels);
    return input.regions.length === 0 || (region !== null && input.regions.includes(region));
  });
  const rank = ({ machine }: (typeof fits)[number]) =>
    machine.id === input.copy ? 0 : home !== null && regionOf(machine.labels) === home ? 1 : 2;
  return fits
    .sort((a, b) => rank(a) - rank(b) || (a.machine.name < b.machine.name ? -1 : a.machine.name > b.machine.name ? 1 : 0))
    .map(({ machine }) => ({ id: machine.id, name: machine.name }));
}

/**
 * The Engine's answer as the drain keeps it: each Service it left on the drained Server for a Volume reads not tried
 * until the drain plans its moves, so a drain that ends before then says so.
 */
export const untried = (report: DrainReport): DrainReport => ({
  ...report,
  services: report.services.map((entry) =>
    entry.result === "stays" && entry.reason.kind === "volume" && entry.reason.server.id === report.server.id && entry.detail === undefined
      ? { ...entry, detail: "not tried" }
      : entry),
});

/** A Volume Run row as a drain's line reads it. */
export type DrainRunRow = {
  readonly id: string;
  readonly kind: string;
  readonly state: VolumeRunState;
  readonly phase: string;
  readonly message: string | null;
  readonly to: string | null;
  /** The Server a Release confirmed it started the Service on again. */
  readonly restarted: MachineId | null;
};

/** How a Volume's line reads on the Server page. */
export type VolumeTone = "moved" | "moving" | "stayed" | "failed" | "waiting";

/** `to` names the Server a move is headed to or reached, when the line is about one. */
export type VolumeLine = { readonly tone: VolumeTone; readonly text: string; readonly to?: string };

const ACTIVE: readonly VolumeRunState[] = ["requested", "running"];

/**
 * One Volume's line, read from the move a drain admitted (`move`) and the Release its end queued (`release`), so it
 * stays current after the drain ended. With no move, the line is the stay the drain named, or, once the drain ended,
 * that it never tried.
 */
export function volumeLine(input: {
  readonly entry: ServiceDrain;
  readonly server: MachineRef;
  readonly move: DrainRunRow | undefined;
  readonly release: DrainRunRow | undefined;
  readonly drainEnded: boolean;
}): VolumeLine | null {
  const { entry, move, release } = input;
  const server = input.server.name;
  if (move === undefined) {
    // A Service Cloud kept for its data has a reason even when it has no one Volume to name.
    if (entry.detail !== undefined) return { tone: "stayed", text: entry.detail };
    if (entry.run === undefined) return null;
    return input.drainEnded ? { tone: "stayed", text: "not tried" } : { tone: "waiting", text: "waiting its turn to move" };
  }
  const volume = entry.volume ?? "the volume";
  const to = move.to ?? "its new server";
  if (move.state === "done") return { tone: "moved", text: `moved to ${to}`, to };
  if (ACTIVE.includes(move.state)) {
    return { tone: "moving", text: move.phase === "committed" ? `starting on ${to}, run ${move.id}` : `moving to ${to}, run ${move.id}`, to };
  }
  const why = move.message ?? `the move ${move.state === "failed" ? "failed" : `ended ${move.state}`}`;
  if (release === undefined) return { tone: "stayed", text: why };
  if (ACTIVE.includes(release.state)) return { tone: "failed", text: `didn't move: ${why}; still releasing, run ${release.id}` };
  if (release.state === "done") {
    // A Release after the commit finishes the move on the new Server; one before it leaves the writer on the drained
    // Server, and starts the Service there only when it kept a Container to start.
    if (move.phase === "committed") return { tone: "moved", text: `moved to ${to} by release run ${release.id} after the move failed: ${why}`, to };
    const restarted = release.restarted === input.server.id ? ` and started it again on ${server}` : "";
    return { tone: "stayed", text: `didn't move: ${why}; release run ${release.id} finished${restarted}${release.message === null ? "" : `; ${release.message}`}` };
  }
  const end = release.message ?? release.state;
  // A Release that ended naming what to do next keeps its own advice.
  const next = /Deploy it again|ployz volume/.test(end) ? "" : `. Where it runs now is unknown; ployz volume release ${volume} finishes it`;
  return { tone: "failed", text: `didn't move: ${why}; release run ${release.id} ended: ${end}${next}` };
}

const EXCLUSIVE = (outcome: ServiceDrain): boolean => {
  switch (outcome.result) {
    case "moved":
      return outcome.moves.every((move) => move.old.kind === "stopped");
    case "old_copies":
      return outcome.ends.every((end) => end.kind === "stopped");
    case "nothing_to_move":
    case "retired":
      return true;
    default:
      return false;
  }
};

/** A move of another Server's copy that left nothing behind, as the Engine's report counts it (`clean_elsewhere`). */
const cleanElsewhere = (entry: ServiceDrain, server: MachineId) => {
  if (entry.result !== "failed") return false;
  const { failure } = entry;
  switch (failure.stage) {
    case "no_destination":
    case "source_too_old":
    case "read_image":
    case "copy_image":
      return failure.from.id !== server;
    case "refused":
      return failure.from !== undefined && failure.from !== null && failure.from.id !== server;
    case "not_serving":
      return failure.from.id !== server && failure.replacement_removed;
    default:
      return false;
  }
};

/** A drain's move that finished: the Server it started the Service on, and the Containers a Server acknowledged it removed. */
export type FinishedMove = { readonly to: MachineRef; readonly removed: readonly ContainerId[] };

/**
 * The report once its moves finished: each Service a finished move took rewritten as moved from the drained Server,
 * what remains there read from `frame`, and `complete` worked out again by the Engine's rule. Only a Container a
 * Server acknowledged removing is left out of what remains, since a frame may still list it; a frame in which the
 * drained Server didn't answer observes nothing there.
 */
export function finalReport(report: DrainReport, finished: ReadonlyMap<string, FinishedMove>, frame: RuntimeWatchView | null): DrainReport {
  const from: MachineRef = { id: report.server.id, name: report.server.name };
  const services = report.services.map((entry): ServiceDrain => {
    const move = entry.run === undefined ? undefined : finished.get(entry.run);
    if (move === undefined) return entry;
    return { service: entry.service, volume: entry.volume, run: entry.run, result: "moved", moves: [{ from, to: move.to, old: { kind: "stopped" } }] };
  });
  const removed = new Set([...finished.values()].flatMap((move) => move.removed));
  const answered = frame !== null && !frame.unanswered.includes(from.id) && frame.machines.some(({ machine }) => machine.id === from.id);
  const remaining: DrainReport["remaining"] = !answered
    ? { kind: "unobserved", error: `Cloud couldn't read ${from.name} after the drain's volumes moved` }
    : (() => {
      const left = frame.services
        .filter((service) => service.containers.some((container) => container.machine_id === from.id && !removed.has(container.container_id)))
        .map((service) => service.identity);
      const unchosen = report.remaining.kind === "observed" ? report.remaining.unchosen.filter((service) => left.includes(service)) : [];
      return { kind: "observed", services: left, unchosen };
    })();
  const rewritten = { ...report, services, remaining };
  return { ...rewritten, complete: reportComplete(rewritten) };
}

/** `DrainReport::complete` over a report Cloud rewrote: the Engine's rule, so the CLI reads the same answer back. */
export function reportComplete(report: Omit<DrainReport, "complete">): boolean {
  const { remaining } = report;
  if (remaining.kind !== "observed" || report.stopped !== null) return false;
  return report.services.every((entry) => {
    const done = cleanElsewhere(entry, report.server.id)
      ? entry.result === "failed" && entry.moves.every((move) => move.old.kind === "stopped")
      : EXCLUSIVE(entry);
    return done && !remaining.services.includes(entry.service);
  });
}
