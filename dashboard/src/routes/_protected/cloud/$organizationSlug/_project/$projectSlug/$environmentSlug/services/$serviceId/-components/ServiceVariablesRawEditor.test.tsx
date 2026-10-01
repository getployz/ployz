// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { ServiceVariablesRawEditor } from "./ServiceVariablesRawEditor";

const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");

afterEach(() => {
  if (originalClipboard) Object.defineProperty(navigator, "clipboard", originalClipboard);
  else Reflect.deleteProperty(navigator, "clipboard");
  cleanup();
  document.body.replaceChildren();
});

it("copies the active format's current draft after editing JSON", async () => {
  const writeText = vi.fn(async () => undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  render(<Harness onApply={() => ({ isPersisted: { promise: Promise.resolve() } })} />);
  fireEvent.change(screen.getByLabelText("Service variables in ENV format"), { target: { value: "A=old" } });
  fireEvent.click(screen.getByRole("button", { name: "Copy ENV" }));
  await waitFor(() => expect(writeText).toHaveBeenLastCalledWith("A=old"));
  fireEvent.click(screen.getByRole("tab", { name: "JSON" }));
  const current = '{"A":"current"}';
  fireEvent.change(screen.getByLabelText("Service variables in JSON format"), { target: { value: current } });
  fireEvent.click(screen.getByRole("button", { name: "Copy JSON" }));
  await waitFor(() => expect(writeText).toHaveBeenLastCalledWith(current));
});

function Harness({ onApply }: { onApply: () => { isPersisted: { promise: Promise<unknown> } } }) {
  const [open, setOpen] = useState(true);
  return <ServiceVariablesRawEditor open={open} onOpenChange={setOpen} variables={[]} onApply={onApply} />;
}

it("a refused save reopens the editor on what was typed, with the Store's reason", async () => {
  const refused = Promise.reject(new Error("web has no variable named MISSING"));
  refused.catch(() => undefined);
  render(<Harness onApply={() => ({ isPersisted: { promise: refused } })} />);
  fireEvent.change(screen.getByLabelText("Service variables in ENV format"), { target: { value: "A=${{ web.MISSING }}" } });
  fireEvent.click(screen.getByRole("button", { name: "Update variables" }));
  await waitFor(() => expect(screen.getByText("web has no variable named MISSING")).toBeTruthy());
  expect(screen.getByLabelText<HTMLTextAreaElement>("Service variables in ENV format").value).toBe("A=${{ web.MISSING }}");
});
