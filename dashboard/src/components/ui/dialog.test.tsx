// @vitest-environment jsdom

import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { AlertDialog, AlertDialogContent, AlertDialogTitle } from "#/components/ui/alert-dialog";
import { Dialog, DialogContent, DialogTitle } from "#/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";

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

/** A row's actions menu whose item opens a dialog, as the variable rows' Rename and Seal do. */
function FromMenuItem({ alert }: { alert?: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger render={<button type="button" aria-label="Actions" />} />
        <DropdownMenuContent>
          <DropdownMenuItem onClick={() => setOpen(true)}>Rename</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {alert ? (
        <AlertDialog open={open} onOpenChange={setOpen}>
          <AlertDialogContent>
            <AlertDialogTitle>Rename?</AlertDialogTitle>
            <input aria-label="Name" autoFocus />
          </AlertDialogContent>
        </AlertDialog>
      ) : (
        <Dialog open={open} onOpenChange={setOpen}>
          <DialogContent>
            <DialogTitle>Rename</DialogTitle>
            <input aria-label="Name" autoFocus />
          </DialogContent>
        </Dialog>
      )}
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

  // The menu item that opened the dialog has unmounted by the time it closes, so focus goes to the menu's trigger.
  // In a browser the dialog keeps focus as the menu closes; jsdom runs the menu's own return to its trigger after the
  // dialog's autofocus, so the test puts focus back in the dialog to start from where a browser is.
  it.each([
    ["a dialog", <FromMenuItem />],
    ["an alert dialog", <FromMenuItem alert />],
  ])("returns focus to the menu's trigger when Escape closes %s opened from a menu item", async (_name, ui) => {
    render(ui);
    const trigger = screen.getByRole("button", { name: "Actions" });
    trigger.focus();
    fireEvent.click(trigger);
    const item = await screen.findByRole("menuitem", { name: "Rename" });
    fireEvent.click(item);
    const input = await screen.findByRole("textbox", { name: "Name" });
    await waitFor(() => expect(screen.queryByRole("menuitem", { name: "Rename" })).toBeNull());
    input.focus();
    expect(document.activeElement).toBe(input);

    fireEvent.keyDown(input, { key: "Escape" });

    await waitFor(() => expect(screen.queryByRole("textbox", { name: "Name" })).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(trigger));
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
    await waitFor(() => expect(document.activeElement).toBe(name));
  });
});
