// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { renderToString } from "react-dom/server";
import { hydrateRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ConnectedAgentList, SessionList } from "./account-sections";

type Sessions = Parameters<typeof SessionList>[0]["sessions"];

const linux = "Mozilla/5.0 (X11; Linux x86_64) Chrome/130.0";
const firefox = "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0";
const session = (id: string, userAgent: string, ipAddress: string | null, lastActiveAt = "2026-10-19T10:00:00Z"): Sessions[number] =>
  ({ id, cli: false, userAgent, ipAddress, createdAt: "2026-10-01T10:00:00Z", lastActiveAt });

const signOutNames = () => screen.getAllByRole("button").map((button) => button.getAttribute("aria-label"));

describe("SessionList", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
    vi.useRealTimers();
  });

  it("names a Sign out after its device alone when no other session shares it", () => {
    render(<SessionList currentSessionId="this" onSignOut={vi.fn()} sessions={[
      session("this", linux, "10.0.0.1"),
      session("laptop", linux, "10.0.0.2"),
      session("browser", firefox, "10.0.0.2"),
    ]} />);

    expect(signOutNames()).toEqual(["Sign out Chrome on Linux", "Sign out Firefox on Linux"]);
  });

  it("tells sessions on one device apart by IP, then by last active, then by position", () => {
    vi.useFakeTimers({ now: new Date("2026-10-19T12:00:00Z"), toFake: ["Date"] });
    const onSignOut = vi.fn();
    render(<SessionList currentSessionId="this" onSignOut={onSignOut} sessions={[
      session("this", linux, "10.0.0.1"),
      session("work", linux, "10.0.0.3"),
      session("recent", linux, "10.0.0.2", "2026-10-19T11:59:00Z"),
      session("older", linux, "10.0.0.2", "2026-10-17T12:00:00Z"),
      session("twin-a", linux, "10.0.0.4"),
      session("twin-b", linux, "10.0.0.4"),
    ]} />);

    const names = signOutNames();
    expect(new Set(names).size).toBe(names.length);
    expect(names[0]).toBe("Sign out Chrome on Linux, 10.0.0.3");
    expect(names[1]).toMatch(/^Sign out Chrome on Linux, 10\.0\.0\.2, active /);
    expect(names[2]).toMatch(/^Sign out Chrome on Linux, 10\.0\.0\.2, active /);
    expect(names.slice(3)).toEqual(["Sign out Chrome on Linux, 10.0.0.4, 1 of 2", "Sign out Chrome on Linux, 10.0.0.4, 2 of 2"]);

    const [, , older] = screen.getAllByRole("button");
    if (older) fireEvent.click(older);
    expect(onSignOut).toHaveBeenCalledExactlyOnceWith("older");
  });
});

describe("ConnectedAgentList", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  const agent = (id: string, name: string | null, organization: string) =>
    ({ id, name, organization, allowedAt: "2026-10-19T10:00:00Z" });

  it("names each Disconnect after its agent, then its Organization, and disconnects that one", () => {
    const onDisconnect = vi.fn();
    render(<ConnectedAgentList onDisconnect={onDisconnect} agents={[
      agent("shop", "Claude", "Shop"),
      agent("lab", "Claude", "Lab"),
      agent("unnamed", null, "Shop"),
    ]} />);

    expect(screen.getAllByRole("button").map((button) => button.getAttribute("aria-label")))
      .toEqual(["Disconnect Claude, Shop", "Disconnect Claude, Lab", "Disconnect Unnamed agent"]);
    fireEvent.click(screen.getByRole("button", { name: "Disconnect Claude, Lab" }));
    expect(onDisconnect).toHaveBeenCalledExactlyOnceWith("lab");
  });
});

describe("Account relative times", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("hydrates without a mismatch when the minute turns between the server render and the browser's", async () => {
    const lists = () => (
      <>
        <SessionList currentSessionId="this" onSignOut={vi.fn()} sessions={[
          session("this", linux, "10.0.0.1"),
          session("recent", linux, "10.0.0.2", "2026-10-19T11:58:40Z"),
          session("older", linux, "10.0.0.2", "2026-10-17T12:00:00Z"),
        ]} />
        <ConnectedAgentList onDisconnect={vi.fn()} agents={[
          { id: "a", name: "Claude", organization: "Shop", allowedAt: "2026-10-19T11:58:40Z" },
        ]} />
      </>
    );
    // The server renders at 11:59:59, when the session was active "1 minute ago".
    vi.useFakeTimers({ now: new Date("2026-10-19T11:59:59Z"), toFake: ["Date"] });
    const container = document.createElement("div");
    container.innerHTML = renderToString(lists());
    document.body.append(container);
    // The browser hydrates at 12:00:21, when it reads "2 minutes ago".
    vi.setSystemTime(new Date("2026-10-19T12:00:21Z"));
    const recoverable = vi.fn();
    const consoleError = vi.spyOn(console, "error");
    await act(async () => { hydrateRoot(container, lists(), { onRecoverableError: recoverable }); });
    expect(recoverable).not.toHaveBeenCalled();
    expect(consoleError).not.toHaveBeenCalled();
    // Once hydrated, the times show, and the Sign out names tell the two sessions on one device apart by them.
    expect(container.textContent).toContain("Active 2 minutes ago");
    expect(container.textContent).toContain("Allowed 2 minutes ago");
    const names = [...container.querySelectorAll("button")].map((button) => button.getAttribute("aria-label"));
    expect(names).toEqual(["Sign out Chrome on Linux, active 2 minutes ago", "Sign out Chrome on Linux, active 2 days ago", "Disconnect Claude"]);
  });
});
