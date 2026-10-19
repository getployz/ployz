// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SessionList } from "./account-sections";

type Sessions = Parameters<typeof SessionList>[0]["sessions"];

const linux = "Mozilla/5.0 (X11; Linux x86_64) Chrome/130.0";
const session = (id: string, userAgent: string, ipAddress: string | null): Sessions[number] =>
  ({ id, cli: false, userAgent, ipAddress, createdAt: "2026-10-01T10:00:00Z", lastActiveAt: "2026-10-19T10:00:00Z" });

describe("SessionList", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it("names each Sign out after its session", () => {
    const onSignOut = vi.fn();
    render(<SessionList currentSessionId="this" onSignOut={onSignOut} sessions={[
      session("this", linux, "10.0.0.1"),
      session("laptop", linux, "10.0.0.2"),
      session("desktop", linux, "10.0.0.3"),
      session("phone", "Mozilla/5.0 (iPhone) Version/18.0 Safari/604.1", null),
    ]} />);

    expect(screen.getByRole("button", { name: "Sign out Chrome on Linux, 10.0.0.3" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Sign out Safari on iOS" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Sign out" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Sign out Chrome on Linux, 10.0.0.2" }));
    expect(onSignOut).toHaveBeenCalledExactlyOnceWith("laptop");
  });
});
