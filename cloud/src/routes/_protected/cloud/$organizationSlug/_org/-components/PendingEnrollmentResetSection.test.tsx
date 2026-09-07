// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PendingEnrollmentResetSection } from "./PendingEnrollmentResetSection";

describe("PendingEnrollmentResetSection", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it.each([
    ["unclaimed", "Organization enrollment unclaimed"],
    ["ready", "Organization enrollment ready"],
  ] as const)(
    "shows %s enrollment without offering reset",
    (status, heading) => {
      render(
        <PendingEnrollmentResetSection status={status} onReset={vi.fn()} />,
      );

      expect(screen.getByText(heading)).toBeTruthy();
      expect(
        screen.queryByRole("button", { name: "Reset founding attempt" }),
      ).toBeNull();
    },
  );

  it("requires stopped-or-erased confirmation and warns about an orphaned Cluster", async () => {
    const onReset = vi.fn().mockResolvedValue(undefined);
    render(
      <PendingEnrollmentResetSection status="pending" onReset={onReset} />,
    );

    expect(screen.getByText("Founding attempt pending")).toBeTruthy();
    fireEvent.click(
      screen.getByRole("button", { name: "Reset founding attempt" }),
    );

    expect(await screen.findByText(/does not erase or destroy/i)).toBeTruthy();
    expect(screen.getByText(/orphaned Cluster/i)).toBeTruthy();
    const reset = screen.getByRole("button", { name: "Reset enrollment" });
    expect((reset as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(
      screen.getByRole("checkbox", {
        name: /old Machine has been stopped or erased/i,
      }),
    );
    expect((reset as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(reset);

    await waitFor(() => {
      expect(onReset).toHaveBeenCalledWith({
        confirmedFounderStoppedOrErased: true,
      });
    });
  });
});
