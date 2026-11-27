// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ConfirmDialog } from "./confirm-dialog";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
});

it("keeps the dialog open on Escape while its confirm runs, and closes on Escape after", async () => {
  let finish = () => {};
  const onOpenChange = vi.fn();
  render(<ConfirmDialog open onOpenChange={onOpenChange} title="Turn off?" actionLabel="Turn off"
    onConfirm={() => new Promise<void>((done) => { finish = done; })} />);

  fireEvent.click(screen.getByRole("button", { name: "Turn off" }));
  fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
  expect(onOpenChange).not.toHaveBeenCalled();

  finish();
  await vi.waitFor(() => expect((screen.getByRole("button", { name: "Turn off" }) as HTMLButtonElement).disabled).toBe(false));
  fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
  expect(onOpenChange).toHaveBeenCalledWith(false);
});
