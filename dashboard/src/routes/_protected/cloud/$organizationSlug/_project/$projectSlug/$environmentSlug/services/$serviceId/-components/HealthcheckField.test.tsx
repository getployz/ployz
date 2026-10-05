// @vitest-environment jsdom
import type { JsonValue } from "@ployz/sdk";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HealthcheckField } from "./HealthcheckField";

const saved = { isPersisted: { promise: Promise.resolve() } };

function field(value: JsonValue) {
  const set = vi.fn((_: JsonValue | null) => saved);
  const view = render(<HealthcheckField value={value} change={undefined} set={set} />);
  return { set, rerender: (next: JsonValue) => view.rerender(<HealthcheckField value={next} change={undefined} set={set} />) };
}

function type(label: string, text: string) {
  const input = screen.getByLabelText(label);
  fireEvent.change(input, { target: { value: text } });
  fireEvent.keyDown(input, { key: "Enter" });
}

describe("HealthcheckField", () => {
  afterEach(cleanup);

  it("adds an HTTP path as a whole check, with the default timeout", () => {
    const { set } = field(null);
    fireEvent.click(screen.getByRole("button", { name: "HTTP path" }));
    expect(screen.getByText("HTTP")).toBeTruthy();
    type("Healthcheck path", "/up");

    expect(set).toHaveBeenCalledExactlyOnceWith({ path: "/up", timeoutSeconds: 300 });
  });

  it("adds a command as a whole check", () => {
    const { set } = field(null);
    fireEvent.click(screen.getByRole("button", { name: "Command" }));
    expect(screen.getByText("CMD")).toBeTruthy();
    type("Healthcheck command", "pg_isready -h 127.0.0.1");

    expect(set).toHaveBeenCalledExactlyOnceWith({ command: "pg_isready -h 127.0.0.1", timeoutSeconds: 300 });
  });

  it("refuses a path without a leading slash and commits nothing", () => {
    const { set } = field(null);
    fireEvent.click(screen.getByRole("button", { name: "HTTP path" }));
    type("Healthcheck path", "up");

    expect(set).not.toHaveBeenCalled();
    expect(screen.getByText("Start the path with /.")).toBeTruthy();
  });

  it("keeps the command when only the timeout changes", () => {
    const { set } = field({ command: "redis-cli ping", timeoutSeconds: 300 });
    type("Healthcheck timeout", "60");

    expect(set).toHaveBeenCalledExactlyOnceWith({ command: "redis-cli ping", timeoutSeconds: 60 });
  });

  it("turns the check off with its ×, and shows the two buttons once it is off", () => {
    const { set, rerender } = field({ path: "/up", timeoutSeconds: 30 });
    expect(screen.getByLabelText("Healthcheck path")).toHaveProperty("value", "/up");
    fireEvent.click(screen.getByRole("button", { name: "Remove healthcheck" }));
    expect(set).toHaveBeenCalledExactlyOnceWith(null);

    rerender(null);
    expect(screen.queryByLabelText("Healthcheck path")).toBeNull();
    expect(screen.queryByLabelText("Healthcheck timeout")).toBeNull();
    expect(screen.getByRole("button", { name: "Command" })).toBeTruthy();
  });

  it("goes back to the buttons when adding is cancelled", () => {
    const { set } = field(null);
    fireEvent.click(screen.getByRole("button", { name: "Command" }));
    fireEvent.keyDown(screen.getByLabelText("Healthcheck command"), { key: "Escape" });

    expect(set).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "HTTP path" })).toBeTruthy();
  });
});
