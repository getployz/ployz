// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { VariableRecord } from "#/modules/variables/variables";
import { VariableAddForm } from "./variable-add-form";

afterEach(cleanup);

const existing: VariableRecord = {
  id: "API_KEY", serviceId: "web", key: "API_KEY", description: null, exported: false,
  value: { type: "plain", value: "old" },
};

function renderForm(onCreateVariable = vi.fn()) {
  render(
    <VariableAddForm
      variables={[existing]}
      onCreateVariable={onCreateVariable}
      onCancel={() => {}}
      allowSealOnCreate
      defaultExported={false}
      supportsExport={false}
      serviceNames={["web"]}
    />,
  );
  return onCreateVariable;
}

describe("VariableAddForm", () => {
  it("adds a plain variable unless Sealed is ticked", () => {
    const onCreateVariable = renderForm();
    fireEvent.change(screen.getByLabelText("Key"), { target: { value: "PORT" } });
    fireEvent.change(screen.getByLabelText("Value"), { target: { value: "8080" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    expect(onCreateVariable).toHaveBeenCalledWith({ key: "PORT", value: "8080", sealed: false, exported: false });
  });

  it("overwriting an existing key keeps the chosen Sealed state", () => {
    const onCreateVariable = renderForm();
    fireEvent.click(screen.getByRole("checkbox", { name: "Sealed" }));
    fireEvent.change(screen.getByLabelText("Key"), { target: { value: "API_KEY" } });
    fireEvent.change(screen.getByLabelText("Value"), { target: { value: "new" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    fireEvent.click(screen.getByRole("button", { name: "Overwrite" }));
    expect(onCreateVariable).toHaveBeenCalledWith({ key: "API_KEY", value: "new", sealed: true, exported: false });
  });

  it("refuses a reference in a sealed value", () => {
    renderForm();
    fireEvent.click(screen.getByRole("checkbox", { name: "Sealed" }));
    fireEvent.change(screen.getByLabelText("Key"), { target: { value: "DB" } });
    fireEvent.change(screen.getByLabelText("Value"), { target: { value: "${{ web.URL }}" } });
    expect(screen.getByText("A sealed value is stored as-is. Untick Sealed to use a reference.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add" }).hasAttribute("disabled")).toBe(true);
  });
});
