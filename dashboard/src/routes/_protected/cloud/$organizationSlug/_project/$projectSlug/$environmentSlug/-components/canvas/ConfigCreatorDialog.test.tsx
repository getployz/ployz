// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ConfigCreatorDialog } from "./ConfigCreatorDialog";

afterEach(cleanup);

it("stays open with its name when the create is refused, leaving no unhandled rejection", async () => {
  let unhandled = 0;
  const record = () => { unhandled += 1; };
  process.on("unhandledRejection", record);
  try {
    const onOpenChange = vi.fn();
    const onCreate = vi.fn(() => Promise.reject(new Error("refused")));
    render(<ConfigCreatorDialog open onOpenChange={onOpenChange} onCreate={onCreate} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "nginx" } });
    fireEvent.click(screen.getByRole("button", { name: "Create config" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Create config" })).toHaveProperty("disabled", false));
    expect(onCreate).toHaveBeenCalledWith("nginx");
    expect(onOpenChange).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Name")).toHaveProperty("value", "nginx");
    await waitFor(() => expect(unhandled).toBe(0));
  } finally {
    process.off("unhandledRejection", record);
  }
});

it("closes once the create is saved", async () => {
  const onOpenChange = vi.fn();
  render(<ConfigCreatorDialog open onOpenChange={onOpenChange} onCreate={() => Promise.resolve()} />);
  fireEvent.click(screen.getByRole("button", { name: "Create config" }));
  await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
});
