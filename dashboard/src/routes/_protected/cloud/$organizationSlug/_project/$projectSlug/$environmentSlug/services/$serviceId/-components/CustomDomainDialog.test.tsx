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

describe("CustomDomainDialog", () => {
  it("saves a blank target as following PORT rather than freezing the hint", async () => {
    const onSubmit = vi.fn();
    render(
      <CustomDomainDialog
        defaultTargetPort={3000}
        onClose={vi.fn()}
        onSubmit={onSubmit}
      />
    );
    fireEvent.change(screen.getByLabelText("Domain"), {
      target: { value: "api.example.com" },
    });
    const save = screen.getByRole("button", { name: "Save route" });
    await waitFor(() => expect(save.hasAttribute("disabled")).toBe(false));
    fireEvent.click(save);
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({ targetPort: null })
      )
    );
  });

  it("enables saving a corrected port without requiring blur", async () => {
    const onSubmit = vi.fn();
    render(
      <CustomDomainDialog
        route={{
          id: crypto.randomUUID(),
          hostname: "api.example.com",
          targetPort: 8080,
        }}
        defaultTargetPort={8080}
        onClose={vi.fn()}
        onSubmit={onSubmit}
      />
    );
    const port = screen.getByLabelText("Target port");
    const save = screen.getByRole("button", { name: "Save route" });
    fireEvent.focus(port);
    fireEvent.change(port, { target: { value: "70000" } });
    await waitFor(() => expect(save.hasAttribute("disabled")).toBe(true));
    expect(screen.queryByText("Enter a port between 1 and 65535.")).toBeNull();
    fireEvent.change(port, { target: { value: "3000" } });
    await waitFor(() => expect(save.hasAttribute("disabled")).toBe(false));
    fireEvent.click(save);
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({ targetPort: 3000 })
      )
    );
  });

  // Saving is optimistic: a failed write rolls back and toasts in the service writer.
  it("closes as soon as the route is submitted", async () => {
    const onClose = vi.fn();
    const onSubmit = vi.fn();

    render(
      <CustomDomainDialog
        defaultTargetPort={8080}
        onClose={onClose}
        onSubmit={onSubmit}
      />
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
      })
    );
  });
});
