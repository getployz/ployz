// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { NotificationSettings } from "#/modules/notifications/notification-types";
import { EMAIL_SETUP_DOCS, LOG_SHIPPING_DOCS, NotificationsSection } from "./notifications-section";

const settings = (mailOn: boolean, shippingOn = true): NotificationSettings => ({
  mailOn,
  shippingOn,
  channels: { uptime: "none", "deploy-failed": "email", "upgrade-failed": "email", crash: "email" },
});

const row = (name: string) => screen.getByRole<HTMLButtonElement>("combobox", { name });

describe("NotificationsSection", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it("shows one row per email type with its channel", () => {
    render(<NotificationsSection settings={settings(true)} onChange={vi.fn()} />);

    expect(screen.getAllByRole("combobox")).toHaveLength(4);
    expect(row("Uptime incidents").textContent).toContain("None");
    expect(row("Failed GitHub deploys").textContent).toContain("Email");
    expect(row("Failed automatic upgrades").textContent).toContain("Email");
    expect(row("Crashed services").textContent).toContain("Email");
    expect(screen.queryByText(/Email isn't set up/u)).toBeNull();
    expect(screen.queryByText(/Cloud log shipping/u)).toBeNull();
  });

  it("saves a new choice for its type only", async () => {
    const onChange = vi.fn();
    render(<NotificationsSection settings={settings(true)} onChange={onChange} />);

    fireEvent.click(row("Failed GitHub deploys"));
    const none = await screen.findByRole("option", { name: "None" });
    fireEvent.mouseMove(none);
    fireEvent.click(none);

    expect(onChange).toHaveBeenCalledExactlyOnceWith({ type: "deploy-failed", channel: "none" });
  });

  it("disables every row and says why when this Cloud has no Mailer", () => {
    render(<NotificationsSection settings={settings(false)} onChange={vi.fn()} />);

    for (const trigger of screen.getAllByRole<HTMLButtonElement>("combobox")) {
      expect(trigger.hasAttribute("data-disabled")).toBe(true);
    }
    expect(screen.getByText(/Email isn't set up on this Cloud/u)).toBeTruthy();
    expect(screen.getByRole("link", { name: "Set up email" }).getAttribute("href")).toBe(EMAIL_SETUP_DOCS);
  });

  it("keeps the crash row, still changeable, and says to set up log shipping when this Cloud keeps no logs", () => {
    render(<NotificationsSection settings={settings(true, false)} onChange={vi.fn()} />);

    expect(row("Crashed services").textContent).toContain("Email");
    expect(row("Crashed services").hasAttribute("data-disabled")).toBe(false);
    expect(screen.getByText(/Crash emails need Cloud log shipping/u)).toBeTruthy();
    expect(screen.getByRole("link", { name: "Set up log shipping" }).getAttribute("href")).toBe(LOG_SHIPPING_DOCS);
    expect(screen.queryByText("When a service exits with an error or runs out of memory.")).toBeNull();
    expect(screen.getByText("When a check goes down, and when it recovers.")).toBeTruthy();
  });
});
