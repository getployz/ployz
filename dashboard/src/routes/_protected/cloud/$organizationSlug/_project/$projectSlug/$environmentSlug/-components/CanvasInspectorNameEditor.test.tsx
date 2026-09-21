// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { Schema } from "effect";
import { CanvasInspectorNameEditor } from "./CanvasInspectorNameEditor";

afterEach(cleanup);

it("submits the rename form once and lets the dialog handle Escape without saving", async () => {
  const rename = vi.fn().mockResolvedValue(undefined);
  render(<CanvasInspectorNameEditor value="api" schema={Schema.String} onRename={rename}
    editTitle="Rename service" editDescription="Choose a name" placeholder="Name" />);
  fireEvent.click(screen.getByRole("button", { name: "api" }));
  const input = await screen.findByRole("textbox", { name: "Rename service" });
  fireEvent.change(input, { target: { value: "worker" } });
  const form = input.closest("form");
  if (!form) throw new Error("Rename must use a native form");
  expect(form.querySelector('button[type="submit"]')).toBeTruthy();
  fireEvent.submit(form);
  await waitFor(() => expect(rename).toHaveBeenCalledExactlyOnceWith("worker"));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  fireEvent.click(screen.getByRole("button", { name: "api" }));
  const reopened = await screen.findByRole("textbox", { name: "Rename service" });
  fireEvent.change(reopened, { target: { value: "discarded" } });
  fireEvent.keyDown(reopened, { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(rename).toHaveBeenCalledTimes(1);
});
