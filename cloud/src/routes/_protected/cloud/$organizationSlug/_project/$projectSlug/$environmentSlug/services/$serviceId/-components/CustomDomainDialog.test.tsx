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

const manualGuidance = {
  kind: "manual" as const,
  title: "Configure DNS separately",
  description: "Save the route before configuring DNS.",
};

function capabilityAction() {
  return <a href="/billing">Review billing</a>;
}

describe("CustomDomainDialog", () => {
  it("copies the exact lease target and closes only after persistence succeeds", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const onClose = vi.fn();
    const onSubmit = vi.fn().mockResolvedValue(undefined);

    render(
      <CustomDomainDialog
        defaultTargetPort={8080}
        guidance={{
          kind: "target",
          target: "tenant.up.ployz.app",
          title: "Configure DNS manually",
          description:
            "Create a CNAME record to this target, or use your provider's ALIAS or ANAME record at the zone apex.",
        }}
        capabilityAction={capabilityAction()}
        onCapabilityRejected={vi.fn().mockResolvedValue(undefined)}
        onClose={onClose}
        onSubmit={onSubmit}
      />,
    );

    expect(screen.getByText("tenant.up.ployz.app")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Copy DNS target" }));
    expect(writeText).toHaveBeenCalledWith("tenant.up.ployz.app");

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

  it("keeps Save available without inventing a DNS target", () => {
    render(
      <CustomDomainDialog
        defaultTargetPort={8080}
        guidance={{
          kind: "manual",
          title: "DNS target unavailable right now",
          description:
            "Runtime evidence is not current, so Ployz cannot show a DNS target. Save the route now and configure DNS separately.",
        }}
        capabilityAction={capabilityAction()}
        onCapabilityRejected={vi.fn().mockResolvedValue(undefined)}
        onClose={vi.fn()}
        onSubmit={vi.fn().mockResolvedValue(undefined)}
      />,
    );

    expect(screen.getByText("DNS target unavailable right now")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Copy DNS target" })).toBeNull();
    expect(
      (screen.getByRole("button", { name: "Save route" }) as HTMLButtonElement)
        .disabled,
    ).toBe(false);
  });

  it("keeps the dialog open and refreshes billing after capability rejection", async () => {
    const onClose = vi.fn();
    const onCapabilityRejected = vi.fn().mockResolvedValue(undefined);

    render(
      <CustomDomainDialog
        defaultTargetPort={8080}
        guidance={manualGuidance}
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
        guidance={manualGuidance}
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
