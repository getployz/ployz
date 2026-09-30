// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
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
