// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DestructiveVolumeEvidence } from "#/modules/operations/destructive-volume-evidence";
import {
  type PreparedVolumeDestruction,
  VolumeDestructionConfirmationDialog,
} from "./volume-destruction-confirmation-dialog";

const availableVolume: DestructiveVolumeEvidence = {
  namespaceId: "production",
  volumeName: "database",
  machineId: "machine-a",
  kind: {
    kind: "provisioned",
    dataset: "ployz/production/database",
    maxSizeBytes: 10_000,
  },
  availability: {
    status: "available",
    usedBytes: 512,
    lastWriteUnixSeconds: 1_700_000_000,
  },
  referencingServices: ["api", "web"],
};

function preparation(
  overrides: Partial<PreparedVolumeDestruction> = {},
): PreparedVolumeDestruction {
  return {
    namespaceId: "production",
    reviews: [
      {
        target: {
          version: 1,
          resourceId: "volume-resource-a",
          namespaceId: "production",
          volumeName: "database",
          machineId: "machine-a",
        },
        evidence: {
          version: 1,
          fingerprint: "volume-a",
          reviewedAt: "2026-07-17T00:00:00.000Z",
          evidence: availableVolume,
        },
      },
    ],
    volumes: [{ evidence: availableVolume, fingerprint: "volume-a" }],
    referencingServices: ["api", "web"],
    fingerprint: "evidence-a",
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  document.body.style.removeProperty("overflow");
});

describe("VolumeDestructionConfirmationDialog", () => {
  it("groups reviewed Service and Volume removals at the Save boundary", async () => {
    render(
      <VolumeDestructionConfirmationDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="production"
        serviceNames={["api", "worker"]}
        actionLabel="Save removals"
        title="Save destructive changes?"
        description="Review every deployed Service and Volume removal before saving."
        callbacks={{
          load: vi.fn().mockResolvedValue(preparation()),
          confirm: vi.fn().mockResolvedValue(undefined),
        }}
      />,
    );

    expect(await screen.findByText("Save destructive changes?")).toBeTruthy();
    const serviceRemovals = screen.getByRole("region", {
      name: "Services to remove",
    });
    expect(within(serviceRemovals).getByText("api")).toBeTruthy();
    expect(within(serviceRemovals).getByText("worker")).toBeTruthy();
    expect(screen.getByText("database")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Save removals" })).toBeTruthy();
  });

  it("blocks confirmation while gathering and presents fresh provisioned evidence", async () => {
    let resolvePreparation!: (value: PreparedVolumeDestruction) => void;
    const prepare = vi.fn(
      () =>
        new Promise<PreparedVolumeDestruction>((resolve) => {
          resolvePreparation = resolve;
        }),
    );
    const confirm = vi.fn().mockResolvedValue(undefined);

    render(
      <VolumeDestructionConfirmationDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="production"
        callbacks={{
          load: prepare,
          confirm,
        }}
      />,
    );

    expect(screen.getByLabelText("Preparing destructive review")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Deploy and delete" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);

    resolvePreparation(preparation());

    expect(await screen.findByText("ployz/production/database")).toBeTruthy();
    expect(screen.getByText("10,000 bytes")).toBeTruthy();
    expect(screen.getByText("512 bytes")).toBeTruthy();
    expect(screen.getByText("machine-a")).toBeTruthy();
    expect(screen.getByText("api")).toBeTruthy();
    expect(screen.getByText("web")).toBeTruthy();

    fireEvent.change(screen.getByLabelText(/Type production to confirm/), {
      target: { value: "production" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Deploy and delete" }));

    await waitFor(() => {
      expect(confirm).toHaveBeenCalledWith(preparation());
    });
  });

  it("identifies silent machines and keeps size and recency unknown", async () => {
    const silentVolume: DestructiveVolumeEvidence = {
      ...availableVolume,
      machineId: "machine-silent",
      availability: { status: "no_answer" },
    };

    render(
      <VolumeDestructionConfirmationDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="production"
        callbacks={{
          load: vi.fn().mockResolvedValue(
            preparation({
              volumes: [{ evidence: silentVolume, fingerprint: "volume-silent" }],
              referencingServices: ["api", "worker", "web"],
            }),
          ),
          confirm: vi.fn().mockResolvedValue(undefined),
        }}
      />,
    );

    expect(await screen.findByText("No answer")).toBeTruthy();
    expect(screen.getAllByText("machine-silent")).toHaveLength(2);
    expect(screen.getAllByText("Unknown")).toHaveLength(2);
    expect(screen.getByText("worker")).toBeTruthy();
    expect(screen.getByText(/did not answer/)).toBeTruthy();
  });

  it("clears the exact namespace phrase when reloaded evidence drifts", async () => {
    const load = vi
      .fn()
      .mockResolvedValueOnce(preparation())
      .mockResolvedValue(
        preparation({
        fingerprint: "evidence-b",
        volumes: [
          {
            fingerprint: "volume-b",
            evidence: {
              ...availableVolume,
              availability: {
                status: "available",
                usedBytes: 1_024,
                lastWriteUnixSeconds: 1_700_000_100,
              },
            },
          },
        ],
        }),
      );

    render(
      <VolumeDestructionConfirmationDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="production"
        callbacks={{
          load,
          confirm: vi.fn().mockResolvedValue(undefined),
        }}
      />,
    );

    const input = await screen.findByLabelText(/Type production to confirm/);
    fireEvent.change(input, { target: { value: "production" } });
    expect(
      (screen.getByRole("button", { name: "Deploy and delete" }) as HTMLButtonElement)
        .disabled,
    ).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Reload evidence" }));

    expect(await screen.findByText("Volume evidence changed")).toBeTruthy();
    expect((input as HTMLInputElement).value).toBe("");
    expect(screen.getByText("1,024 bytes")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Deploy and delete" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("cannot submit after an evidence error and recovers only through reload", async () => {
    const load = vi
      .fn()
      .mockRejectedValueOnce(new Error("machine RPC timed out"))
      .mockResolvedValue(preparation());

    render(
      <VolumeDestructionConfirmationDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="production"
        callbacks={{
          load,
          confirm: vi.fn().mockResolvedValue(undefined),
        }}
      />,
    );

    expect(await screen.findByText("machine RPC timed out")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "Deploy and delete" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    expect(
      (screen.getByLabelText(/Type production to confirm/) as HTMLInputElement)
        .disabled,
    ).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Reload evidence" }));

    await waitFor(() => {
      expect(load).toHaveBeenCalledTimes(2);
      expect(
        (screen.getByLabelText(/Type production to confirm/) as HTMLInputElement)
          .disabled,
      ).toBe(false);
    });
  });

  it("does not gather again when a parent rerenders with a new callbacks object", async () => {
    const prepare = vi.fn().mockResolvedValue(preparation());
    const confirm = vi.fn().mockResolvedValue(undefined);
    const { rerender } = render(
      <VolumeDestructionConfirmationDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="production"
        callbacks={{
          load: prepare,
          confirm,
        }}
      />,
    );

    expect(await screen.findByText("ployz/production/database")).toBeTruthy();
    rerender(
      <VolumeDestructionConfirmationDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="production"
        callbacks={{
          load: prepare,
          confirm,
        }}
      />,
    );

    await waitFor(() => expect(prepare).toHaveBeenCalledOnce());
  });

  it("replaces drifted evidence and requires the namespace phrase again", async () => {
    const changed = preparation({
      fingerprint: "evidence-b",
      volumes: [
        {
          fingerprint: "volume-b",
          evidence: {
            ...availableVolume,
            availability: {
              status: "available",
              usedBytes: 2_048,
              lastWriteUnixSeconds: 1_700_000_200,
            },
          },
        },
      ],
    });
    const confirm = vi.fn().mockResolvedValue({
      state: "review_updated_evidence",
      preparation: changed,
    });

    render(
      <VolumeDestructionConfirmationDialog
        open
        onOpenChange={vi.fn()}
        confirmPhrase="production"
        callbacks={{
          load: vi.fn().mockResolvedValue(preparation()),
          confirm,
        }}
      />,
    );

    const input = await screen.findByLabelText(/Type production to confirm/);
    fireEvent.change(input, { target: { value: "production" } });
    fireEvent.click(screen.getByRole("button", { name: "Deploy and delete" }));

    expect(await screen.findByText("Volume evidence changed")).toBeTruthy();
    expect(screen.getByText("2,048 bytes")).toBeTruthy();
    expect((input as HTMLInputElement).value).toBe("");
    expect(
      (screen.getByRole("button", { name: "Deploy and delete" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });
});
