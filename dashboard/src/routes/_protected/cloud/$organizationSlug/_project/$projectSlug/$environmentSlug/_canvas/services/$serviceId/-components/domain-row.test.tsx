// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DomainTitle, PublicDomainRow, storeStatusView, type DomainStatusView } from "./domain-row";

const records = [{ type: "CNAME" as const, name: "www", value: "acme.ployz.app" }];

const row = (view: DomainStatusView, live = false, pending = false, dnsRecords = records) => (
  <PublicDomainRow
    organizationSlug="acme"
    title={
      <DomainTitle hostname="www.acme.com" live={live} />
    }
    label="www.acme.com"
    portLabel="Port 8080"
    view={view}
    dnsRecords={dnsRecords}
    changed={false}
    pending={pending}
    onEdit={vi.fn()}
    onDelete={vi.fn()}
  />
);

describe("PublicDomainRow", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it("opens a live domain and says nothing else", () => {
    render(row({ icon: null, phrase: null, action: null }, true, false, []));

    expect(screen.getByRole("link").getAttribute("href")).toBe("https://www.acme.com");
    expect(screen.getByText("→ Port 8080")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Edit www.acme.com" })).toBeTruthy();
  });

  it("dims a domain the Store has not answered yet and offers no edit until it does", () => {
    render(row({ icon: null, phrase: null, action: null }, false, true));

    expect(screen.getByText("Saving…")).toBeTruthy();
    expect(document.querySelector("[aria-busy='true']")?.textContent).toContain("www.acme.com");
    expect(screen.queryByRole("button", { name: "Edit www.acme.com" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Remove www.acme.com" })).toBeNull();
  });

  it("stays quiet when the status is unknown", () => {
    render(row({ icon: null, phrase: null, action: null }, false, false, []));

    expect(screen.queryByRole("link")).toBeNull();
    expect(screen.getByText("→ Port 8080")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Show DNS records" })).toBeNull();
  });

  it("shows the DNS record to add only when asked, with a copyable name and value", () => {
    render(row({ icon: null, phrase: "Waiting for DNS update", action: "dns" }));

    expect(screen.queryByRole("link")).toBeNull();
    expect(screen.queryByText("CNAME")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show DNS records" }));
    expect(screen.getByText("CNAME")).toBeTruthy();
    expect(screen.getByText("www")).toBeTruthy();
    expect(screen.getByText("acme.ployz.app")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Copy CNAME name" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Copy CNAME value" })).toBeTruthy();
  });

  it("offers the DNS records even when the status names another cause", () => {
    render(row({ icon: null, phrase: "Port 80 is closed on 203.0.113.7", action: null }));

    fireEvent.click(screen.getByRole("button", { name: "Show DNS records" }));
    expect(screen.getByText("acme.ployz.app")).toBeTruthy();
  });
});

describe("storeStatusView", () => {
  it("shows the Store's reason and offers its DNS records, but no action to wait for a deploy", () => {
    expect(storeStatusView({ status: "needs_attention", reason: "Waiting for DNS", action: { type: "dns", records } }))
      .toMatchObject({ phrase: "Waiting for DNS", action: "dns" });
    expect(storeStatusView({ status: "needs_attention", reason: "No Server receives traffic", action: { type: "add_server" } }).action)
      .toBe("servers");
    expect(storeStatusView({ status: "setting_up", reason: "Live after your next deploy", action: { type: "deploy" } }).action)
      .toBeNull();
  });
});
