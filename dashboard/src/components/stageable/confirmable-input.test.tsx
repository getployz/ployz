// @vitest-environment jsdom

import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";

describe("ConfirmableInput", () => {
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
});
