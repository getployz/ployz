import { describe, expect, it } from "vitest";
import type { DeployOperation, OperationRow, MachineId, ContainerId } from "@ployz/sdk";
import { resolvedServiceSpecFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { deploymentProgressForEvent } from "./deployment-progress";

function row(index: number): OperationRow {
  const spec = resolvedServiceSpecFixture();
  spec.container.environment = { SECRET: "never-publish" };
  return { index, machine_id: `machine-${index}` as MachineId, machine_name: `server-${index}`, service_name: spec.name, display_name: `api-${index}`, operation: { type: "run_container", machine_id: `machine-${index}` as MachineId, spec, skip_health_monitor: false }, status: { type: "pending" } };
}
describe("deployment progress browser boundary", () => {
  it("retains concurrent phases, identities and health deadlines without resolved secrets", () => {
    const rows: OperationRow[] = [
      { ...row(0), status: { type: "completed" } },
      { ...row(1), status: { type: "running", phase: { type: "waiting_for_health", container_id: "container-1" as ContainerId, health: "starting", elapsed_ms: 12000, deadline_ms: 60000 } } },
      { ...row(2), status: { type: "running", phase: { type: "creating_container" } } },
    ];
    const progress = deploymentProgressForEvent({ type: "progress", completed: 1, total: 3, rows }, rows);
    expect(progress.rows[1]).toMatchObject({ machineName: "server-1", phase: "waiting_for_health", elapsedMs: 12000, deadlineMs: 60000, health: "starting" });
    expect(progress.rows[2]?.phase).toBe("creating_container");
    expect(JSON.stringify(progress)).not.toContain("never-publish");
    expect(JSON.stringify(progress)).not.toContain("environment");
  });
  it("keeps completed work, failure and unattempted work separate, including compensation", () => {
    const spec = resolvedServiceSpecFixture();
    const replacement = { machine_id: "machine-1" as MachineId, old_container_id: "old" as ContainerId, spec, skip_health_monitor: false };
    const op: DeployOperation = { type: "replace_container", ...replacement };
    const rows = [row(0), { ...row(1), operation: op }, row(2)] as const;
    const progress = deploymentProgressForEvent({ type: "outcome", outcome: {
      type: "failed", completed: [rows[0].operation], unexecuted: [rows[2].operation],
      failed: { type: "replacement_health", operation: replacement, error: { type: "health", container_id: "new" as ContainerId, failure: { type: "timed_out" } }, compensation: { type: "stop_first", stop_new_container: { type: "stopped" }, restart_old_container: { type: "failed", error: { type: "machine", action: "StartContainer", error: { code: "unavailable", message: "never-publish", details: { secret: "never-publish" } } } } } },
    } }, rows);
    expect(progress.rows.map((r) => r.status)).toEqual(["completed", "failed", "unexecuted"]);
    expect(progress.compensation).toEqual(["Replacement container stopped", "Previous container restart failed: StartContainer: unavailable"]);
    expect(progress.rows[1]?.error).toBe("Health check timed out");
    expect(JSON.stringify(progress)).not.toContain("never-publish");
  });
});
