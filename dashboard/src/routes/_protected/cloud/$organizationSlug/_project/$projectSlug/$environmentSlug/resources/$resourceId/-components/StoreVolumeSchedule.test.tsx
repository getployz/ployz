// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_POLICY, type SnapshotPolicy } from "#/modules/volume-copies/snapshot-schedule";
import type { Snapshot } from "#/modules/volume-copies/volume-copies";
import { ScheduleDialog } from "./StoreVolumeSchedule";

const hourly = (id: string, hoursAgo: number, kept = false): Snapshot => ({
  id, at: new Date(Date.UTC(2026, 9, 12, 12 - hoursAgo)), kind: "hourly", note: null, kept, bytes: 1e6, losesBytes: 0,
});
const SNAPSHOTS = [hourly("h3", 0), hourly("h2", 1, true), hourly("h1", 2)];

function dialog(policy: SnapshotPolicy, snapshots: Snapshot[] = SNAPSHOTS) {
  const onSave = vi.fn((_policy: SnapshotPolicy) => Promise.resolve(true));
  const onOpenChange = vi.fn();
  render(<ScheduleDialog volume="data" policy={policy} snapshots={snapshots} pending={false} onOpenChange={onOpenChange} onSave={onSave} />);
  return { onSave, onOpenChange, view: within(screen.getByRole("dialog")) };
}

describe("ScheduleDialog", () => {
  afterEach(cleanup);

  it("saves the toggles and steppers it shows, and closes once saved", async () => {
    const { onSave, onOpenChange, view } = dialog(DEFAULT_POLICY);
    expect(view.getByText("Snapshots · data")).toBeTruthy();
    expect(view.getByText(/Saving takes effect now\./)).toBeTruthy();
    fireEvent.click(view.getByRole("switch", { name: "Before" }));
    fireEvent.click(view.getByRole("button", { name: "More deploys kept" }));
    fireEvent.click(view.getByRole("button", { name: "Fewer space limit" }));
    fireEvent.click(view.getByRole("button", { name: "Save" }));
    expect(onSave).toHaveBeenCalledWith({ ...DEFAULT_POLICY, before: true, keepDeploys: 6, capGb: 9 });
    await vi.waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
  });

  it("resets keep to the cadence's default when Every changes, and hides it when off", () => {
    const { view } = dialog(DEFAULT_POLICY);
    expect(view.queryByLabelText("Snapshots kept")).toBeNull();
    fireEvent.click(view.getByRole("button", { name: "Day" }));
    expect(view.getByLabelText("Snapshots kept").textContent).toBe("7");
    expect(view.getByText("7 days")).toBeTruthy();
    fireEvent.click(view.getByRole("button", { name: "Hour" }));
    expect(view.getByLabelText("Snapshots kept").textContent).toBe("24");
  });

  it("warns what Save deletes now, never counting a kept snapshot", () => {
    const { onSave, view } = dialog({ ...DEFAULT_POLICY, every: "hour", keep: 2 });
    expect(view.getByText(/Saving takes effect now\./)).toBeTruthy();
    fireEvent.click(view.getByRole("button", { name: "Fewer snapshots kept" }));
    expect(view.getByText("Saving deletes 1 snapshot now.")).toBeTruthy();
    fireEvent.click(view.getByRole("button", { name: "Save" }));
    expect(onSave).toHaveBeenCalledWith({ ...DEFAULT_POLICY, every: "hour", keep: 1 });
  });

  it("stops the steppers at their limits", () => {
    const { view } = dialog({ ...DEFAULT_POLICY, capGb: 20, keepDeploys: 1 });
    expect(view.getByRole("button", { name: "More space limit" }).hasAttribute("disabled")).toBe(true);
    expect(view.getByRole("button", { name: "Fewer deploys kept" }).hasAttribute("disabled")).toBe(true);
  });
});
