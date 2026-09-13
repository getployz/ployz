// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CustomDomainDialog } from "./CustomDomainDialog";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  document.body.style.removeProperty("overflow");
});

function capabilityAction() {
  return <a href="/billing">Review billing</a>;
}

describe("CustomDomainDialog", () => {
  it("closes only after route persistence succeeds", async () => {
    const onClose = vi.fn();
    const onSubmit = vi.fn().mockResolvedValue(undefined);

    render(
      <CustomDomainDialog
        defaultTargetPort={8080}
        capabilityAction={capabilityAction()}
        onCapabilityRejected={vi.fn().mockResolvedValue(undefined)}
        onClose={onClose}
        onSubmit={onSubmit}
      />,
    );

    fireEvent.change(screen.getByLabelText("Domain"), {
      target: { value: "api.example.com" },
    });
    fireEvent.change(screen.getByLabelText("Target port"), {
      target: { value: "3000" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save route" }));

    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(onSubmit).toHaveBeenCalledWith(
      expect.objectContaining({
        id: expect.any(String),
        hostname: "api.example.com",
        targetPort: 3000,
      }),
    );
  });

  it("keeps the dialog open and refreshes billing after capability rejection", async () => {
    const onClose = vi.fn();
    const onCapabilityRejected = vi.fn().mockResolvedValue(undefined);

    render(
      <CustomDomainDialog
        defaultTargetPort={8080}
        capabilityAction={capabilityAction()}
        onCapabilityRejected={onCapabilityRejected}
        onClose={onClose}
        onSubmit={vi.fn().mockRejectedValue({
          _tag: "CustomDomainCapabilityError",
          message: "Custom-domain access is no longer available.",
        })}
      />,
    );

    fireEvent.change(screen.getByLabelText("Domain"), {
      target: { value: "api.example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save route" }));

    await waitFor(() => {
      expect(screen.getByText("Custom domain access changed")).toBeTruthy();
    });
    expect(onClose).not.toHaveBeenCalled();
    expect(onCapabilityRejected).toHaveBeenCalledOnce();
    expect(screen.getByRole("link", { name: "Review billing" })).toBeTruthy();
  });

  it("keeps the dialog open when persistence fails", async () => {
    const onClose = vi.fn();
    render(
      <CustomDomainDialog
        defaultTargetPort={8080}
        capabilityAction={capabilityAction()}
        onCapabilityRejected={vi.fn().mockResolvedValue(undefined)}
        onClose={onClose}
        onSubmit={vi.fn().mockRejectedValue(new Error("Write failed."))}
      />,
    );

    fireEvent.change(screen.getByLabelText("Domain"), {
      target: { value: "api.example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save route" }));

    await waitFor(() => expect(screen.getByText("Write failed.")).toBeTruthy());
    expect(onClose).not.toHaveBeenCalled();
  });
});
