import type { PreparationEvent } from "@ployz/sdk";
import type { PreparationProgress } from "./deployment-progress";

/** One attempt's authorized output. Provider errors and rejection dumps are never logs. */
export function preparationProgressCollector() {
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
        const output = decoder.decode(Uint8Array.from(event.Build.Output), { stream: true });
        current = { ...current, phase: "build", output };
      } else return null;
      return current;
    },
  };
}
