// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ProjectNameRow } from "./store-settings";

afterEach(cleanup);

it("holds the project name while a rename saves", () => {
  const onRename = vi.fn(() => new Promise<void>(() => {}));
  render(<ProjectNameRow project="shop" onRename={onRename} />);
  const input = screen.getByRole("textbox", { name: "Name" });
  fireEvent.change(input, { target: { value: "store" } });
  fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

  expect(onRename).toHaveBeenCalledExactlyOnceWith("store");
  expect(input.hasAttribute("disabled")).toBe(true);
  expect(screen.getByLabelText("Saving")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Confirm" })).toBeNull();
  fireEvent.keyDown(input, { key: "Enter" });
  expect(onRename).toHaveBeenCalledTimes(1);
});
