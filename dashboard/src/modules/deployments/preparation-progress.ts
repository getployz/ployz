import type { PreparationEvent } from "@ployz/sdk";
import type { PreparationProgress } from "./deployment-progress";

export const BUILD_OUTPUT_LIMIT = 64 * 1024;
/** One attempt's bounded, authorized output. Provider errors and rejection dumps are never logs. */
export function preparationProgressCollector() {
  let remaining = BUILD_OUTPUT_LIMIT;
  let count = 0;
  const decoder = new TextDecoder();
  let current: PreparationProgress = { phase: "selection", serviceId: null, machineId: null, machineName: null, message: null, output: "", outputTruncated: false };
  return {
    current: () => ({ ...current, output: "" }),
    event(event: PreparationEvent): PreparationProgress | null {
      current = { ...current, output: "" };
      if (event === "Transfer") current = { ...current, phase: "transfer", message: "Delivering images" };
      else if ("Selected" in event) current = { ...current, machineId: event.Selected.machine.id, machineName: event.Selected.machine.name, message: "Builder selected" };
      else if ("phase" in event) current = { ...current, outputTruncated: true };
      else if ("Build" in event && "Stage" in event.Build) current = { ...current, phase: "build", message: event.Build.Stage };
      else if ("Build" in event && "Output" in event.Build) {
        if (remaining <= 0 || count >= 256) {
          if (current.outputTruncated) return null;
          current = { ...current, outputTruncated: true };
        } else {
          const bytes = event.Build.Output;
          const truncated = bytes.length > remaining;
          let output = decoder.decode(Uint8Array.from(bytes.slice(0, remaining)), { stream: true });
          while (Buffer.byteLength(output) > remaining) output = output.slice(0, -1);
          remaining -= Buffer.byteLength(output);
          count += 1;
          current = { ...current, phase: "build", output, outputTruncated: current.outputTruncated || truncated };
        }
      } else return null;
      return current;
    },
  };
}
