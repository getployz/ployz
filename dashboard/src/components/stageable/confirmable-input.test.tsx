// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";

describe("ConfirmableInput", () => {
  afterEach(cleanup);
  it("renders a suffix inside the input group", () => {
    render(
      <ConfirmableInput
        aria-label="CPU limit"
        value="2"
        suffix="vCPU"
        onValueChange={vi.fn()}
      />,
    );

    expect(screen.getByText("vCPU")).toBeTruthy();
  });

  it("marks the input group as changed when diffed from the baseline", () => {
    render(
      <ConfirmableInput
        aria-label="Root Directory"
        value="/apps/api"
        isChanged
        onValueChange={vi.fn()}
      />,
    );

    const input = screen.getByLabelText("Root Directory");
    const inputGroup = input.closest('[data-slot="input-group"]');

    expect(inputGroup?.getAttribute("data-changed")).toBe("true");
  });

  it("waits for its check when it renames, whatever focus does", () => {
    const onConfirm = vi.fn();
    render(<ConfirmableInput aria-label="Name" commit="check" value="shop-2" isDirty onValueChange={vi.fn()} onConfirm={onConfirm} />);
    fireEvent.blur(screen.getByLabelText("Name"));
    expect(onConfirm).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(onConfirm).toHaveBeenCalledOnce();
  });
});
