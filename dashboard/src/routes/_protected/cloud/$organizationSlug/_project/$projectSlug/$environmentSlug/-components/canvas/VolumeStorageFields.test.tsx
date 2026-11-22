// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { VolumeStorageFields } from "./VolumeStorageFields";

afterEach(cleanup);

it("accepts exact byte limits and explains unsupported input until corrected", () => {
  const field = (sizeGB: string) => <VolumeStorageFields managed sizeGB={sizeGB} onManagedChange={() => {}} onSizeChange={() => {}} />;
  const view = render(field(".001"));
  for (const value of [".001", "1.073741824", "9007199.254740991"]) {
    view.rerender(field(value));
    expect((screen.getByRole("spinbutton") as HTMLInputElement).checkValidity()).toBe(true);
    expect(screen.queryByRole("alert")).toBeNull();
  }
  view.rerender(field("1e0"));
  expect(screen.getByRole("alert").textContent).toContain("decimal GB");
  expect(screen.getByRole("spinbutton").getAttribute("aria-invalid")).toBe("true");
  view.rerender(field("0.5"));
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByRole("spinbutton").getAttribute("aria-invalid")).toBeNull();
});
