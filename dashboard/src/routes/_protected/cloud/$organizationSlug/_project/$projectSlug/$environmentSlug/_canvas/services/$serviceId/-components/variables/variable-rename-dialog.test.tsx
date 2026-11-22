// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { VariableRenameDialog } from "./variable-rename-dialog";

afterEach(cleanup);

it("renames once on a double-click, while the dialog closes", () => {
  const onRename = vi.fn();
  render(<VariableRenameDialog variableKey="API_KEY" open onOpenChange={vi.fn()} onRename={onRename} />);
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "API_TOKEN" } });
  const rename = screen.getByRole("button", { name: "Rename" });
  fireEvent.click(rename);
  fireEvent.click(rename);

  expect(onRename).toHaveBeenCalledExactlyOnceWith("API_TOKEN", false);
});
