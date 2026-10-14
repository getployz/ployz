import { stepAt, steps, type TimeWindow } from "#/modules/uptime/uptime";
import type { Sample } from "./metrics-model";

export function onGrid(samples: readonly Sample[], window: TimeWindow): (number | null)[] {
  const buckets = new Map<number, { sum: number; count: number }>();
  for (const sample of samples) {
    if (sample.at < window.from || sample.at > window.to) continue;
    const step = stepAt(window, sample.at);
    const bucket = buckets.get(step) ?? { sum: 0, count: 0 };
    buckets.set(step, { sum: bucket.sum + sample.value, count: bucket.count + 1 });
  }
  return steps(window).map((at) => {
    const bucket = buckets.get(at);
    return bucket ? bucket.sum / bucket.count : null;
  });
}

const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

/** Formats every value in the unit `top` reads in, so one axis never mixes MB and GB. */
export function bytesIn(top: number) {
  let unit = 0;
  while (Math.abs(top) >= 1024 ** (unit + 1) && unit < BYTE_UNITS.length - 1) unit += 1;
  return (bytes: number) => {
    const value = bytes / 1024 ** unit;
    return `${Number(value.toFixed(Math.abs(value) < 10 && unit > 0 ? 1 : 0))} ${BYTE_UNITS[unit]}`;
  };
}

export const bytesText = (bytes: number) => bytesIn(bytes)(bytes);

export const rateText = (bytesPerSecond: number) => `${bytesText(bytesPerSecond)}/s`;

export const rateIn = (top: number) => {
  const bytes = bytesIn(top);
  return (bytesPerSecond: number) => `${bytes(bytesPerSecond)}/s`;
};

/** A share of one core, so 0.5 vCPU says "50%", with the decimals a small value needs. */
export const cpuText = (vcpus: number) => {
  const percent = vcpus * 100;
  return `${Number(percent.toFixed(percent >= 10 ? 0 : percent >= 1 ? 1 : 2))}%`;
};

/** Says every CPU tick with the decimals the axis's step needs, so a quiet Service's axis never reads 0%, 0%, 0%. */
export function cpuIn(top: number) {
  const step = (top * 100) / 3;
  const decimals = Math.min(2, Math.max(0, -Math.floor(Math.log10(step))));
  return (vcpus: number) => `${Number((vcpus * 100).toFixed(decimals))}%`;
}
