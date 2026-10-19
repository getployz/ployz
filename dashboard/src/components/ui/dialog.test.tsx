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

function Handoff() {
  const [create, setCreate] = useState(false);
  const [config, setConfig] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setCreate(true)}>Create</button>
      <Dialog open={create} onOpenChange={setCreate}>
        <DialogContent>
          <DialogTitle>Create</DialogTitle>
          <button type="button" autoFocus onClick={() => { setCreate(false); setConfig(true); }}>Config</button>
        </DialogContent>
      </Dialog>
      <Dialog open={config} onOpenChange={setConfig}>
        <DialogContent>
          <DialogTitle>Create config</DialogTitle>
          <input aria-label="Name" autoFocus />
        </DialogContent>
      </Dialog>
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

  it("leaves focus in the next dialog when one dialog hands off to another", async () => {
    render(<Handoff />);
    const opener = screen.getByRole("button", { name: "Create" });
    opener.focus();
    fireEvent.click(opener);
    const config = await screen.findByRole("button", { name: "Config" });
    await waitFor(() => expect(document.activeElement).toBe(config));

    fireEvent.click(config);

    await waitFor(() => expect(screen.queryByRole("button", { name: "Config" })).toBeNull());
    const name = screen.getByRole("textbox", { name: "Name" });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(document.activeElement).toBe(name);
  });
});
