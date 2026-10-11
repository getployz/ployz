import { causeText, incidentSpan, steps, type TimeWindow } from "#/modules/uptime/uptime";
import type { UptimeIncidentEntry } from "#/modules/uptime/uptime-status";

// Ployz doesn't collect Service metrics yet. This dev-only story invents them from the check's incidents, so the
// Monitoring tab can show how they would line up in dev builds; it is deleted when real metrics land.

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** Per-replica limits from the Service spec, drawn as dashed lines. */
export const LIMITS = { cpu: 2, memoryGb: 1, diskGb: 10 } as const;

/** One step of every metric. Requests are per minute, network in KB/s. */
export type MetricsRow = {
  readonly edgeMs: number | null;
  readonly ok: number;
  readonly errors: number;
  readonly cpu: number;
  readonly memoryGb: number;
  readonly inKbps: number;
  readonly outKbps: number;
  readonly diskGb: number;
};

/** A stable value in [0, 1) per seed and slot. */
function noise(seed: number, slot: number) {
  let t = Math.imul(seed ^ slot, 2_654_435_761) >>> 0;
  t = Math.imul(t ^ (t >>> 15), t | 1);
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
  return ((t ^ (t >>> 14)) >>> 0) / 4_294_967_296;
}

/** Every metric at each step of the window, bent by whichever incident the step falls in. */
export function previewMetrics(incidents: readonly UptimeIncidentEntry[], window: TimeWindow, now: number): MetricsRow[] {
  return steps(window).map((at) => {
    const slot = Math.floor(at / (5 * MINUTE));
    const jitter = (seed: number) => noise(seed, slot);
    const dayPhase = ((at % DAY) / DAY) * 2 * Math.PI;
    const load = 0.6 + 0.4 * Math.sin(dayPhase - Math.PI / 2) * -1;
    const incident = incidents.find((candidate) => {
      const span = incidentSpan(candidate, now);
      return at >= span.from - MINUTE && at < span.to;
    });
    const cause = incident ? causeText(incident) : null;
    // A slow leak towards the limit in the six hours before a 502 incident, as if the deploy that preceded it leaked.
    const leaking = incidents.find((candidate) => {
      const opened = Date.parse(candidate.openedAt);
      return causeText(candidate) === "502" && at >= opened - 6 * HOUR && at < opened;
    });
    const leak = leaking ? 1 - (Date.parse(leaking.openedAt) - at) / (6 * HOUR) : 0;

    let ok = 30 + 70 * load + jitter(1) * 12;
    let errors = jitter(2) < 0.08 ? 1 : 0;
    let cpu = 0.18 + 0.5 * load + jitter(3) * 0.12;
    let memoryGb = 0.38 + 0.06 * ((at % (8 * HOUR)) / (8 * HOUR)) + jitter(4) * 0.02 + leak * 0.55;
    let edgeMs: number | null = 38 + 25 * load + jitter(5) * 18;
    if (cause === "502") {
      errors = ok * 0.92;
      ok *= 0.08;
      memoryGb = LIMITS.memoryGb * (0.97 + jitter(6) * 0.03);
      cpu = 1.1 + jitter(7) * 0.4;
      edgeMs = 1700 + jitter(8) * 400;
    } else if (cause === "Timeout") {
      errors = ok * 0.55;
      ok *= 0.45;
      cpu = LIMITS.cpu * (0.96 + jitter(9) * 0.04);
      edgeMs = 6500 + jitter(10) * 3000;
    } else if (cause === "DNS") {
      ok *= 0.03;
      errors = 0;
      cpu = 0.05 + jitter(11) * 0.03;
      edgeMs = null;
    }
    const total = ok + errors;
    return {
      edgeMs,
      ok,
      errors,
      cpu,
      memoryGb,
      inKbps: total * (1.4 + jitter(12) * 0.4),
      outKbps: total * (9 + jitter(13) * 3),
      diskGb: 3.1 + ((at - (now - 90 * DAY)) / (90 * DAY)) * 1.4,
    };
  });
}
