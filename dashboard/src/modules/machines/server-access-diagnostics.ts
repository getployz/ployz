import type { DialTiming } from "@ployz/sdk";

type DialTimingBase = {
  readonly totalMs: number;
  readonly outcome: "answered" | "cancelled" | "timed_out" | "refused" | "unanswered";
  readonly sampled: boolean;
  readonly correlation: string;
};

export type DialTimingInput = DialTimingBase & { readonly stage: "cloud_dial" | "set_management_client" };

export function dialCorrelation(): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(8)), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function shouldLogDialTiming(input: DialTimingInput): boolean {
  return input.sampled || input.totalMs >= 2_000 || input.outcome !== "answered";
}

export function dialTimingFields(input: DialTimingInput) {
  const fields = {
    stage: input.stage,
    totalMs: Math.round(input.totalMs),
    outcome: input.outcome,
    correlation: input.correlation,
  };
  return fields;
}

export function nativeDialTimingFields(timing: DialTiming, cloudOutcome?: DialTimingInput["outcome"]) {
  return {
    stage: "native_dial" as const,
    correlation: timing.correlation,
    totalMs: timing.totalMs,
    endpointBindMs: timing.endpointBindMs ?? null,
    irohConnectMs: timing.irohConnectMs ?? null,
    proofMs: timing.proofMs ?? null,
    http2ChannelMs: timing.http2ChannelMs ?? null,
    probeDescribeContractMs: timing.probeDescribeContractMs ?? null,
    entryDescribeContractMs: timing.entryDescribeContractMs ?? null,
    unfinishedStage: timing.unfinishedStage ?? null,
    pathKind: timing.pathKind,
    outcome: timing.outcome,
    stoppedBy: timing.outcome === "cancelled" && cloudOutcome === "timed_out" ? "cloud_deadline" : timing.stoppedBy ?? null,
  };
}

/** Whether the `sequence`th dial is one of the one in `rate` that gets sampled timing. */
export const isSampledDial = (sequence: number, rate: number) => sequence % rate === 0;

let dials = 0;

export function sampleDialTiming(rate: number): boolean {
  dials += 1;
  return isSampledDial(dials, rate);
}
