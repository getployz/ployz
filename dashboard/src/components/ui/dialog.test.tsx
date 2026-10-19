// @vitest-environment jsdom

import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { AlertDialog, AlertDialogContent, AlertDialogTitle } from "#/components/ui/alert-dialog";
import { Dialog, DialogContent, DialogTitle } from "#/components/ui/dialog";

function MountedOnOpen({ alert }: { alert?: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>Open</button>
      {open && alert ? (
        <AlertDialog open onOpenChange={setOpen}>
          <AlertDialogContent>
            <AlertDialogTitle>Delete?</AlertDialogTitle>
            <input aria-label="Name" autoFocus />
          </AlertDialogContent>
        </AlertDialog>
      ) : null}
      {open && !alert ? (
        <Dialog open onOpenChange={setOpen}>
          <DialogContent>
            <DialogTitle>Invite</DialogTitle>
            <input aria-label="Name" autoFocus />
          </DialogContent>
        </Dialog>
      ) : null}
    </>
  );
}

function AlwaysMounted() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>Open</button>
      <AlertDialog open={open} onOpenChange={setOpen}>
        <AlertDialogContent>
          <AlertDialogTitle>Leave?</AlertDialogTitle>
          <input aria-label="Name" autoFocus />
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

describe("dialogs opened without a Trigger", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it.each([
    ["a dialog mounted open", <MountedOnOpen />],
    ["an alert dialog mounted open", <MountedOnOpen alert />],
    ["an alert dialog toggled open", <AlwaysMounted />],
  ])("returns focus to the button that opened %s when Escape closes it", async (_name, ui) => {
    render(ui);
    const opener = screen.getByRole("button", { name: "Open" });
    opener.focus();
    fireEvent.click(opener);
    const input = await screen.findByRole("textbox", { name: "Name" });
    await waitFor(() => expect(document.activeElement).toBe(input));

    fireEvent.keyDown(input, { key: "Escape" });

    await waitFor(() => expect(screen.queryByRole("textbox", { name: "Name" })).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(opener));
  });
});
