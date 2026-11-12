import type { MachineId } from "@ployz/sdk";
import { Option, Schema } from "effect";
import type { StepAnswer, VolumeRunView } from "#/modules/volume-run/volume-run";

export const EXPIRED_NOTE = "Approval expired. Nothing new can start.";
export const FINISHING_NOTE = "Finishing what started.";

/** How long past an approval's signed expiry a Server still takes a step from it, for clocks that disagree. */
export const CLOCK_SKEW_MS = 2 * 60 * 1000;

const ApprovedBody = Schema.fromJsonString(Schema.Struct({
  expires_at: Schema.Number,
  workflow: Schema.Struct({
    from: Schema.optional(Schema.String),
    to: Schema.optional(Schema.String),
    mirrors: Schema.optional(Schema.Array(Schema.String)),
    writer: Schema.optional(Schema.String),
    mirror: Schema.optional(Schema.String),
    machine: Schema.optional(Schema.String),
    machines: Schema.optional(Schema.Array(Schema.String)),
  }),
}));

/** What one Server named by the approval answered: each step it took or refused, or nothing yet. */
export type ServerProgress = {
  readonly machine: MachineId;
  readonly name: string;
  readonly steps: readonly { readonly step: string; readonly accepted: boolean }[];
};

/**
 * How a run asked under an approval stands: its Servers' answers, and once the approval expired, whether the run still
 * finishes what a Server already took (the Servers let a started workflow finish), or nothing new can start.
 */
export type ApprovalProgress = {
  /** The last moment a Server takes a new step under the approval: its signed expiry, plus the skew Servers allow. */
  readonly until: number;
  readonly note: typeof EXPIRED_NOTE | typeof FINISHING_NOTE | null;
  readonly servers: readonly ServerProgress[];
};

const ACTIVE: readonly VolumeRunView["state"][] = ["requested", "running"];

/**
 * Pure: the progress of `run` at `now`, or null for a run that carries no approval. `names` names a Server Cloud knows by
 * id; one that never answered keeps its id. A Server's latest answer to a step stands for that step.
 */
export function approvalProgress(
  run: Pick<VolumeRunView, "args" | "state" | "answers">, now: number, names: (id: MachineId) => string | undefined = () => undefined,
): ApprovalProgress | null {
  const approval = "approval" in run.args ? run.args.approval : undefined;
  if (approval === undefined) return null;
  const body = Schema.decodeUnknownOption(ApprovedBody)(approval.body);
  if (Option.isNone(body)) return null;
  const { workflow, expires_at: expiresAt } = body.value;
  // SAFETY: an approval names its Servers by their Machine IDs, which each Server checks against its own.
  const named = [workflow.from, workflow.writer, workflow.machine, workflow.to, workflow.mirror, ...(workflow.mirrors ?? []), ...(workflow.machines ?? [])]
    .flatMap((id) => (id === undefined ? [] : [id as MachineId]));
  const parties = [...new Set([...named, ...run.answers.map((answer) => answer.machine)])];
  const servers = parties.map((machine): ServerProgress => {
    const heard = run.answers.filter((answer) => answer.machine === machine);
    const latest = new Map<string, StepAnswer>();
    for (const answer of heard) latest.set(answer.step, answer);
    return {
      machine,
      name: heard.at(-1)?.name ?? names(machine) ?? machine,
      steps: [...latest.values()].map(({ step, accepted }) => ({ step, accepted })),
    };
  });
  const took = run.answers.some((answer) => answer.accepted);
  const until = expiresAt + CLOCK_SKEW_MS;
  const note = now <= until || run.state === "done" ? null
    : ACTIVE.includes(run.state) && took ? FINISHING_NOTE
    : EXPIRED_NOTE;
  return { until, note, servers };
}

const STEP_WORDS = new Map([
  ["writer", "write"],
  ["mirror", "mirror"],
  ["none", "delete its copy"],
  ["size", "resize"],
  ["seed", "restore"],
]);

/** One Server's line: "web-1 took write and mirror", "web-2 refused delete its copy", or "web-3 hasn't answered". */
export function serverLine(server: ServerProgress): string {
  if (server.steps.length === 0) return `${server.name} hasn't answered`;
  const took = server.steps.filter((step) => step.accepted).map((step) => STEP_WORDS.get(step.step) ?? step.step);
  const refused = server.steps.filter((step) => !step.accepted).map((step) => STEP_WORDS.get(step.step) ?? step.step);
  return [took.length > 0 ? `took ${took.join(" and ")}` : null, refused.length > 0 ? `refused ${refused.join(" and ")}` : null]
    .filter((part) => part !== null)
    .reduce((line, part, at) => `${line}${at === 0 ? " " : ", "}${part}`, server.name);
}
