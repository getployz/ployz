// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ServiceVariablesRawEditor } from "./ServiceVariablesRawEditor";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
});

it("a refused save keeps the dialog open with the text and the Store's reason", async () => {
  const onOpenChange = vi.fn();
  const refused = Promise.reject(new Error("web has no variable named MISSING"));
  refused.catch(() => undefined);
  render(<ServiceVariablesRawEditor open onOpenChange={onOpenChange} variables={[]}
    onApply={() => ({ isPersisted: { promise: refused } })} />);
  const text = screen.getByLabelText("Service variables in ENV format");
  fireEvent.change(text, { target: { value: "A=${{ web.MISSING }}" } });
  fireEvent.click(screen.getByRole("button", { name: "Update variables" }));
  await waitFor(() => expect(screen.getByText("web has no variable named MISSING")).toBeTruthy());
  expect(onOpenChange).not.toHaveBeenCalledWith(false);
  expect((screen.getByLabelText("Service variables in ENV format") as HTMLTextAreaElement).value).toBe("A=${{ web.MISSING }}");
});
