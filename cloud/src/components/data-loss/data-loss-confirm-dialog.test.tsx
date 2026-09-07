// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MachineId } from "@ployz/sdk";
import { directVolumeDataLoss, type DataLossList } from "#/modules/runtime/data-loss-confirm";
import {
  DataLossConfirmDialog,
  MachineRemoveDataLossDialog,
  TeardownDataLossDialog,
  VolumeRemoveDataLossDialog,
} from "./data-loss-confirm-dialog";

const machineA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId;
const machineB = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" as MachineId;

const teardownList: DataLossList = {
  rust: [
    { kind: "docker_volume", id: { machine_id: machineA, name: "data" } },
    { kind: "docker_volume", id: { machine_id: machineB, name: "data" } },
  ],
  cloud: [
    { kind: "environment", name: "acme/web/production" },
    { kind: "project", name: "acme/web" },
  ],
};

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  document.body.style.removeProperty("overflow");
});

describe("DataLossConfirmDialog", () => {
  it("is the modal volume remove, machine remove, and teardown all use", async () => {
    const load = vi.fn().mockResolvedValue(teardownList);
    const confirm = vi.fn().mockResolvedValue(undefined);

    for (const Dialog of [
      VolumeRemoveDataLossDialog,
      MachineRemoveDataLossDialog,
      TeardownDataLossDialog,
    ]) {
      cleanup();
      render(
        <Dialog
          open
          onOpenChange={vi.fn()}
          confirmPhrase="web"
          callbacks={{ load, confirm }}
        />,
      );

      const rust = await screen.findByRole("region", {
        name: "Named Data Loss",
      });
      expect(within(rust).getAllByText("docker_volume")).toHaveLength(2);
      expect(within(rust).getByText(`data on ${machineA}`)).toBeTruthy();
      expect(within(rust).getByText(`data on ${machineB}`)).toBeTruthy();
      expect(within(rust).queryByText("data", { exact: true })).toBeNull();
    }
  });

  it("shows Cloud row-loss in the same modal and does not send those names to rust", async () => {
    const confirm = vi.fn().mockResolvedValue(undefined);
    render(
      <TeardownDataLossDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="web"
        callbacks={{
          load: vi.fn().mockResolvedValue(teardownList),
          confirm,
        }}
      />,
    );

    const cloud = await screen.findByRole("region", {
      name: "Cloud records to remove (not sent to rust)",
    });
    expect(within(cloud).getByText("environment")).toBeTruthy();
    expect(within(cloud).getByText("acme/web/production")).toBeTruthy();
    expect(within(cloud).getByText("project")).toBeTruthy();
    expect(within(cloud).getByText("acme/web")).toBeTruthy();

    fireEvent.change(screen.getByLabelText(/Type web to confirm/), {
      target: { value: "web" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Tear down" }));

    await waitFor(() => {
      expect(confirm).toHaveBeenCalledWith(teardownList.rust);
    });
    expect(confirm.mock.calls[0]?.[0]).not.toEqual(
      expect.arrayContaining([
        expect.objectContaining({ name: "acme/web/production" }),
      ]),
    );
  });

  it("shows DIRECT volume identities to a human before confirm", async () => {
    const volumes = [
      { machine_id: machineA, name: "pg-data" },
      { machine_id: machineB, name: "uploads" },
    ];
    const confirm = vi.fn().mockResolvedValue(undefined);

    render(
      <VolumeRemoveDataLossDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="production"
        callbacks={{
          load: vi.fn().mockResolvedValue(directVolumeDataLoss(volumes)),
          confirm,
        }}
      />,
    );

    expect(await screen.findByText(`pg-data on ${machineA}`)).toBeTruthy();
    expect(screen.getByText(`uploads on ${machineB}`)).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Remove volumes" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);

    fireEvent.change(screen.getByLabelText(/Type production to confirm/), {
      target: { value: "production" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Remove volumes" }));

    await waitFor(() => {
      expect(confirm).toHaveBeenCalledWith([
        { kind: "docker_volume", id: volumes[0] },
        { kind: "docker_volume", id: volumes[1] },
      ]);
    });
  });

  it("re-renders CASCADE with missing identities from rust and stays open", async () => {
    const onOpenChange = vi.fn();
    const confirm = vi.fn().mockResolvedValue({
      state: "missing_identities",
      identities: [
        { kind: "docker_volume", id: { machine_id: machineA, name: "new" } },
      ],
    });

    render(
      <MachineRemoveDataLossDialog
        open
        onOpenChange={onOpenChange}
        confirmPhrase="node-1"
        callbacks={{
          load: vi.fn().mockResolvedValue({
            rust: [
              {
                kind: "docker_volume",
                id: { machine_id: machineA, name: "old" },
              },
            ],
            cloud: [{ kind: "machine", name: machineA }],
          }),
          confirm,
        }}
      />,
    );

    expect(await screen.findByText(`old on ${machineA}`)).toBeTruthy();
    fireEvent.change(screen.getByLabelText(/Type node-1 to confirm/), {
      target: { value: "node-1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Remove machine" }));

    expect(await screen.findByText(`new on ${machineA}`)).toBeTruthy();
    expect(screen.getByText(`old on ${machineA}`)).toBeTruthy();
    expect(screen.getByText("Data Loss changed")).toBeTruthy();
    expect(
      (screen.getByLabelText(/Type node-1 to confirm/) as HTMLInputElement).value,
    ).toBe("");
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("does not echo the load into confirm without the typed phrase", async () => {
    const confirm = vi.fn();
    render(
      <DataLossConfirmDialog
        open
        onOpenChange={vi.fn()}
        title="Confirm Data Loss?"
        confirmPhrase="web"
        callbacks={{
          load: vi.fn().mockResolvedValue(teardownList),
          confirm,
        }}
      />,
    );

    await screen.findByText(`data on ${machineA}`);
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(confirm).not.toHaveBeenCalled();
  });
});
