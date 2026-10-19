// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { trimmedString } from "#/lib/schema";
import { ServiceRegistryCredentialSingleFieldEditor } from "./ServiceRegistryCredentialSingleFieldEditor";

afterEach(cleanup);

function setup() {
  const onCommit = vi.fn(() => ({ isPersisted: { promise: Promise.resolve() } }));
  const onClose = vi.fn();
  render(<ServiceRegistryCredentialSingleFieldEditor schema={trimmedString({ minLength: 1 })} secretLabel="Token" onCommit={onCommit} onClose={onClose} />);
  const field = screen.getByLabelText("Token");
  fireEvent.change(field, { target: { value: "ghp_secret" } });
  return { onCommit, onClose, field };
}

it("saves nothing when Cancel takes the focus: the token being discarded stays out of the Store", async () => {
  const test = setup();
  // The editor's own Cancel, beside the field (the field's check mode has its own, by icon).
  const cancel = screen.getByText("Cancel");
  await act(async () => {
    fireEvent.blur(test.field, { relatedTarget: cancel });
    fireEvent.click(cancel);
  });
  expect(test.onClose).toHaveBeenCalledOnce();
  expect(test.onCommit).not.toHaveBeenCalled();
});

it("saves the token on Enter", async () => {
  const test = setup();
  fireEvent.keyDown(test.field, { key: "Enter" });
  await waitFor(() => expect(test.onCommit).toHaveBeenCalledWith("ghp_secret"));
});
