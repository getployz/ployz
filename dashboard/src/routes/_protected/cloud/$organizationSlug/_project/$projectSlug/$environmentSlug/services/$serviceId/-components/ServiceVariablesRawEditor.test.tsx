// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, it } from "vitest";
import { ServiceVariablesRawEditor } from "./ServiceVariablesRawEditor";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
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
