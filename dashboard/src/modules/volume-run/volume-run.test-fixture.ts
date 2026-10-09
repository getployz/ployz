import type { LeaseRecord, MachineId, VolumeCopy } from "@ployz/sdk";
import type { Member, VolumeRunView } from "#/modules/volume-run/volume-run";

const snapshot = { name: "ployz-1", guid: "11", created_unix_seconds: 1_790_000_000 };
export const lease = (cycle: LeaseRecord["cycle"], number = 4): LeaseRecord => ({ lease: number, pos: { seq: 4, round: 0, sub: 5 }, cycle });

/** Each copy a Machine can answer with, by what it is mid-run. */
export const COPIES = {
  writer: { kind: "root", writer: { phase: "idle" }, readonly: false, newest: snapshot },
  switching: { kind: "root", writer: { phase: "frozen", guid: "11" }, readonly: true, newest: snapshot },
  handed: { kind: "root", writer: { phase: "handed", guid: "11" }, readonly: true, newest: snapshot },
  mirror: { kind: "slot", mirror: { phase: "idle" }, readonly: true, newest: snapshot, resume_token: null },
  final: { kind: "slot", mirror: { phase: "final", guid: "11" }, readonly: true, newest: snapshot, resume_token: null },
  stale: { kind: "slot", mirror: { phase: "handed_in", guid: "11" }, readonly: true, newest: snapshot, resume_token: null },
  promoting: { kind: "slot", mirror: { phase: "promoting" }, readonly: true, newest: snapshot, resume_token: null },
  stopping: { kind: "root", writer: { phase: "stopping" }, readonly: false, newest: snapshot },
  thawing: { kind: "root", writer: { phase: "thawing" }, readonly: true, newest: snapshot },
  behind: { kind: "slot", mirror: { phase: "idle" }, readonly: true, newest: { ...snapshot, guid: "10" }, resume_token: null },
  promoted: { kind: "root", writer: { phase: "idle" }, readonly: true, newest: snapshot },
} satisfies Record<string, VolumeCopy>;

export type Copy = keyof typeof COPIES | "empty" | "unanswered";

/** One Machine's answer for the Volume, named by its Server. */
export function member(server: string, copy: Copy, options: { pool?: boolean; cycle?: LeaseRecord["cycle"]; lease?: number } = {}): Member {
  const machine = { id: server.padEnd(32, "0") as MachineId, name: server };
  if (copy === "unanswered") return { machine, answered: false };
  const view = { copy: copy === "empty" ? null : (COPIES[copy] ?? null), lease: options.lease !== undefined ? lease(options.cycle ?? "closed", options.lease) : options.cycle === undefined ? null : lease(options.cycle) };
  return { machine, address: "fdcc::1", answered: true, pool: options.pool ?? copy !== "empty", view };
}

export const run = (kind: VolumeRunView["kind"], args: VolumeRunView["args"], state: VolumeRunView["state"]): VolumeRunView => ({
  id: `${kind}-${state}`, volume_id: "v1", volume_name: "data", kind, args, orphan: false, state, lease: 1, message: null,
  created_at: "2026-10-09T00:00:00.000Z", updated_at: "2026-10-09T00:00:00.000Z", finished_at: null,
});
