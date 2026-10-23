import type { DialTiming } from "@ployz/sdk";

type DialTimingBase = {
  readonly totalMs: number;
  readonly outcome: "answered" | "cancelled" | "timed_out" | "refused" | "unanswered";
  readonly sampled: boolean;
};

export type DialTimingInput = DialTimingBase & (
  | { readonly stage: "cloud_dial"; readonly dialMs: number }
  | { readonly stage: "set_management_client"; readonly setMs: number }
);

export function shouldLogDialTiming(input: DialTimingInput): boolean {
  return input.sampled || input.totalMs >= 2_000 || input.outcome !== "answered";
}

export function dialTimingFields(input: DialTimingInput) {
  const fields = {
    stage: input.stage,
    totalMs: Math.round(input.totalMs),
    outcome: input.outcome,
  };
  return input.stage === "cloud_dial"
    ? { ...fields, dialMs: Math.round(input.dialMs) }
    : { ...fields, setMs: Math.round(input.setMs) };
}

/** Keep native diagnostics inside Cloud's known log schema. */
export function nativeDialTimingFields(timing: DialTiming) {
  return {
    stage: "native_dial" as const,
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
  };
}

const sampleRate = (() => {
  const rate = Number(process.env["PLOYZ_DIAL_SAMPLE_N"] ?? 100);
  return Number.isSafeInteger(rate) && rate >= 1 && rate <= 10_000 ? rate : 100;
})();
let sequence = 0;

export function sampleDialTiming(): boolean {
  sequence += 1;
  return sequence % sampleRate === 0;
}
