import type { MachineId, VolumeCopy } from "@ployz/sdk";
import type { Member, VolumeRunView } from "#/modules/volume-run/volume-run";

const snapshot = { name: "w-1-4", guid: "11", created_unix_seconds: 1_790_000_000 };
const older = { ...snapshot, name: "w-1-3", guid: "10" };

/** Each copy a Machine can answer with. */
export const COPIES = {
  writer: { kind: "root", readonly: false, newest: snapshot, epoch: 1, seed: false },
  cut: { kind: "root", readonly: true, newest: snapshot, epoch: 1, seed: false },
  seed: { kind: "root", readonly: false, newest: null, epoch: 0, seed: true },
  rootWithoutSeed: { kind: "root", readonly: false, newest: null, epoch: 0 } as VolumeCopy,
  mirror: { kind: "slot", slot: "1", readonly: true, newest: snapshot, resume_token: null, pull: null },
  final: { kind: "slot", slot: "1", readonly: true, newest: { ...snapshot, name: "f-1" }, resume_token: null, pull: null },
  behind: { kind: "slot", slot: "1", readonly: true, newest: older, resume_token: null, pull: null },
  pulling: { kind: "slot", slot: "1", readonly: true, newest: older, resume_token: null, pull: { target: snapshot.name, failure: null } },
} satisfies Record<string, VolumeCopy>;

export type Copy = keyof typeof COPIES | "empty" | "unanswered";

/** One Machine's answer for the Volume, named by its Server. `epoch` numbers a root's run of writes. */
export function member(server: string, copy: Copy, options: { pool?: boolean; epoch?: number } = {}): Member {
  const machine = { id: server.padEnd(32, "0") as MachineId, name: server };
  if (copy === "unanswered") return { machine, answered: false };
  const answer: VolumeCopy | null = copy === "empty" ? null : COPIES[copy];
  const view = { copy: answer?.kind === "root" && options.epoch !== undefined ? { ...answer, epoch: options.epoch } : answer };
  return { machine, address: "fdcc::1", answered: true, pool: options.pool ?? copy !== "empty", view };
}

export const run = (kind: VolumeRunView["kind"], args: VolumeRunView["args"], state: VolumeRunView["state"]): VolumeRunView => ({
  id: `${kind}-${state}`, volume_id: "v1", volume_name: "data", kind, args, orphan: false, state, epoch: 1, message: null,
  created_at: "2026-10-09T00:00:00.000Z", updated_at: "2026-10-09T00:00:00.000Z", finished_at: null,
});
