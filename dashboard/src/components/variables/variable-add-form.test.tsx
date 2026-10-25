// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { VariableRecord } from "#/modules/variables/variables";
import { VariableAddForm } from "./variable-add-form";
import type { SealedControls } from "./variables-panel";

afterEach(cleanup);

const existing: VariableRecord = {
  id: "API_KEY", serviceId: "web", key: "API_KEY", description: null, exported: false,
  value: { type: "plain", value: "old" },
};

function renderForm(onCreateVariable = vi.fn(), state: SealedControls["state"] = "ready") {
  render(
    <VariableAddForm
      variables={[existing]}
      onCreateVariable={onCreateVariable}
      onCancel={() => {}}
      sealing={{ state, addServer: <a href="/servers">Add server</a> }}
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

  it("checks a sealed value before it leaves the page while sealing is ready", () => {
    const onCreateVariable = renderForm(vi.fn(), "ready");
    fireEvent.click(screen.getByRole("checkbox", { name: "Sealed" }));
    fireEvent.change(screen.getByLabelText("Key"), { target: { value: "PORT" } });
    fireEvent.change(screen.getByLabelText("Value"), { target: { value: "8080" } });
    expect(screen.getByText("PORT can't be sealed: Ployz reads it to route traffic.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add" }).hasAttribute("disabled")).toBe(true);

    fireEvent.change(screen.getByLabelText("Key"), { target: { value: "STRIPE_SECRET" } });
    fireEvent.change(screen.getByLabelText("Value"), { target: { value: "sk_live_51Hx" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    expect(onCreateVariable).toHaveBeenCalledWith({ key: "STRIPE_SECRET", value: "sk_live_51Hx", sealed: true, exported: false });
  });

  it("waits for a value to seal without calling the empty field a problem", () => {
    const onCreateVariable = renderForm(vi.fn(), "ready");
    fireEvent.click(screen.getByRole("checkbox", { name: "Sealed" }));
    fireEvent.change(screen.getByLabelText("Key"), { target: { value: "TOKEN" } });
    expect(screen.getByRole("button", { name: "Add" }).hasAttribute("disabled")).toBe(true);
    expect(screen.queryByText("A sealed value can't be empty.")).toBeNull();
    fireEvent.submit(screen.getByRole("button", { name: "Add" }));
    expect(onCreateVariable).not.toHaveBeenCalled();
  });

  it("disables Sealed until a server joins, linking to add one", () => {
    const onCreateVariable = renderForm(vi.fn(), "no_server");
    const sealed = screen.getByRole("checkbox", { name: "Sealed" });
    expect(sealed.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByText(/Needs a server\./u)).toBeTruthy();
    expect(screen.getByRole("link", { name: "Add server" }).getAttribute("href")).toBe("/servers");

    fireEvent.click(sealed);
    fireEvent.change(screen.getByLabelText("Key"), { target: { value: "LOG_LEVEL" } });
    fireEvent.change(screen.getByLabelText("Value"), { target: { value: "info" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    expect(onCreateVariable).toHaveBeenCalledWith({ key: "LOG_LEVEL", value: "info", sealed: false, exported: false });
  });
});
