// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { settingError, type SettingSchema } from "#/modules/config-store/catalog";
import { ServiceSettingInput } from "./ServiceSettingInput";

describe("ServiceSettingInput comparison copy", () => {
  afterEach(cleanup);

  it("labels a Saved comparison as Saved rather than Deployed", () => {
    render(
      <ServiceSettingInput
        ariaLabel="Replicas"
        value="3"
        isChanged
        baselineLabel="Saved"
        baselineValue="2"
        onCommit={vi.fn()}
      />,
    );

    expect(screen.getByLabelText("Replicas").getAttribute("title")).toBe(
      "Saved: 2",
    );
  });

  it("keeps what the user is typing when the value changes elsewhere, and says so", () => {
    const field = (value: string) => <ServiceSettingInput ariaLabel="Replicas" value={value} isChanged={false} onCommit={vi.fn()} />;
    const { rerender } = render(field("2"));
    fireEvent.change(screen.getByLabelText("Replicas"), { target: { value: "5" } });
    rerender(field("3"));

    expect((screen.getByLabelText("Replicas") as HTMLInputElement).value).toBe("5");
    expect(screen.getByText(/Changed elsewhere to 3/)).toBeTruthy();
  });

  it("follows a value changed elsewhere while untouched", () => {
    const field = (value: string) => <ServiceSettingInput ariaLabel="Replicas" value={value} isChanged={false} onCommit={vi.fn()} />;
    const { rerender } = render(field("2"));
    rerender(field("3"));

    expect((screen.getByLabelText("Replicas") as HTMLInputElement).value).toBe("3");
  });
});

describe("ServiceSettingInput validation", () => {
  afterEach(cleanup);
  const replicas: SettingSchema = { title: "Replicas", description: "", type: "integer", minimum: 0, maximum: 50 };

  // A number input would read "abc" as blank, which unsets: the text must reach validation as typed.
  it.each(["-1", "0.5", "1e9", "abc"])("refuses %s with the reason and commits nothing", (raw) => {
    const onCommit = vi.fn();
    render(<ServiceSettingInput ariaLabel="Replicas" inputMode="numeric" value="1" isChanged={false}
      validate={(next) => settingError(replicas, next)} onCommit={onCommit} />);
    const input = screen.getByLabelText("Replicas");
    fireEvent.change(input, { target: { value: raw } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onCommit).not.toHaveBeenCalled();
    expect(input.getAttribute("aria-invalid")).toBe("true");
    expect(screen.getByRole("alert").textContent).toBe("Enter a whole number from 0 to 50.");
  });
});
