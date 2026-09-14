// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";
import { CopyButton } from "./copy-button";

beforeEach(() => {
  Object.defineProperty(navigator, "clipboard", { get: () => undefined, configurable: true });
});

function clipboardWith(writeText: Clipboard["writeText"]): Clipboard {
  return Object.assign(new EventTarget(), {
    read: async () => [],
    readText: async () => "",
    write: async () => {},
    writeText,
  });
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  Reflect.deleteProperty(navigator, "clipboard");
});

describe("CopyButton", () => {
  it("confirms only after the clipboard resolves, and does not confirm a changed value", async () => {
    let complete!: () => void;
    const writeText = vi.fn(() => new Promise<void>((resolve) => { complete = resolve; }));
    vi.spyOn(navigator, "clipboard", "get").mockReturnValue(clipboardWith(writeText));
    const view = render(<CopyButton value="first" label="Copy command" />);
    fireEvent.click(screen.getByRole("button", { name: "Copy command" }));
    expect(screen.getByRole("status").textContent).toBe("");
    await act(async () => complete());
    expect(writeText).toHaveBeenCalledWith("first");
    expect(screen.getByRole("status").textContent).toBe("Copied to clipboard");
    view.rerender(<CopyButton value="second" label="Copy command" />);
    expect(screen.getByRole("status").textContent).toBe("");
  });

  it("reports clipboard rejection without claiming success", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("denied"));
    vi.spyOn(navigator, "clipboard", "get").mockReturnValue(clipboardWith(writeText));
    const showError = vi.spyOn(toast, "error");
    render(<CopyButton value="command" />);
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(showError).toHaveBeenCalledWith("Couldn't copy to the clipboard. Try again."));
    expect(screen.getByRole("status").textContent).toBe("");
    expect(screen.getByRole("button").hasAttribute("disabled")).toBe(false);
  });
});
