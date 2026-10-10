// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { MountEditor } from "./MountEditor";

afterEach(cleanup);
function deferred() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const choice = { id: "service-id", name: "web", refusal: null, directory: "/data" };

it("keeps the pending selection and refused directory, allows retry and ignores cancelled completion", async () => {
  const first = deferred();
  const retry = deferred();
  const submit = vi.fn().mockReturnValueOnce({ isPersisted: first }).mockReturnValueOnce({ isPersisted: retry });
  const closed = vi.fn();
  const props = { label: "Service" as const, initial: { counterpart: choice.id, directory: "/attempt", editing: false }, submit, onClose: closed };
  const view = render(<MountEditor {...props} choices={[choice]} />);
  fireEvent.click(screen.getByRole("button", { name: "Mount" }));
  view.rerender(<MountEditor {...props} choices={[]} />);
  expect(screen.getByRole("combobox", { name: "Service" }).textContent).toContain("web");
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Saving…" }).disabled).toBe(true);
  await act(async () => first.reject(new Error("Network failed")));
  expect(screen.getByRole("alert").textContent).toContain("Try again");
  expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Directory" }).value).toBe("/attempt");
  fireEvent.click(screen.getByRole("button", { name: "Mount" }));
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(closed).toHaveBeenCalledTimes(1);
  view.rerender(<MountEditor key="new-attempt" {...props} choices={[choice]} initial={{ ...props.initial, directory: "/new-attempt" }} />);
  await act(async () => retry.resolve());
  expect(closed).toHaveBeenCalledTimes(1);
  expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Directory" }).value).toBe("/new-attempt");
  expect(submit).toHaveBeenCalledTimes(2);
});

it("submits a changed directory by keyboard and keeps local validation visible", () => {
  const submit = vi.fn().mockReturnValue(null);
  const closed = vi.fn();
  render(<MountEditor label="Volume" choices={[choice]} initial={{ counterpart: choice.id, directory: "relative", editing: true }} submit={submit} onClose={closed} />);
  fireEvent.click(screen.getByRole("button", { name: "Save directory" }));
  expect(screen.getByRole("alert").textContent).toContain("absolute");
  expect(submit).not.toHaveBeenCalled();
  fireEvent.change(screen.getByRole("textbox", { name: "Directory" }), { target: { value: "/new" } });
  const form = screen.getByRole("button", { name: "Save directory" }).closest("form");
  if (!form) throw new Error("Mount form missing");
  fireEvent.submit(form);
  expect(submit).toHaveBeenCalledWith(choice.id, "/new");
  expect(closed).toHaveBeenCalledOnce();
});

it("lets the picker handle Escape and closes only on an unmodified Escape inside the form", async () => {
  const closed = vi.fn();
  render(<MountEditor label="Service" choices={[choice]} initial={{ counterpart: choice.id, directory: "/data", editing: false }}
    submit={() => null} onClose={closed} />);
  fireEvent.click(screen.getByRole("combobox", { name: "Service" }));
  const option = await screen.findByRole("option", { name: "web" });
  fireEvent.keyDown(option, { key: "Escape" });
  expect(closed).not.toHaveBeenCalled();
  const directory = screen.getByRole("textbox", { name: "Directory" });
  fireEvent.keyDown(directory, { key: "Escape", shiftKey: true });
  expect(closed).not.toHaveBeenCalled();
  fireEvent.keyDown(directory, { key: "Escape" });
  expect(closed).toHaveBeenCalledOnce();
});
