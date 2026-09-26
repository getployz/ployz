// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PublicDomainStatus } from "#/modules/services/public-domain-status";
import { DomainTitle, PublicDomainRow } from "./domain-row";

const row = (status: PublicDomainStatus) => (
  <PublicDomainRow
    organizationSlug="acme"
    title={
      <DomainTitle hostname="www.acme.com" live={status.kind === "live"} />
    }
    label="www.acme.com"
    portLabel="Port 8080"
    status={status}
    dnsRecords={[{ type: "CNAME", name: "www", value: "acme.ployz.app" }]}
    changed={false}
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
    render(row({ kind: "live" }));

    expect(screen.getByRole("link").getAttribute("href")).toBe("https://www.acme.com");
    expect(screen.getByText("→ Port 8080")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Edit www.acme.com" })).toBeTruthy();
  });

  it("stays quiet when the status is unknown", () => {
    render(row({ kind: "unknown" }));

    expect(screen.queryByRole("link")).toBeNull();
    expect(screen.getByText("→ Port 8080")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Show DNS records" })).toBeNull();
  });

  it("shows the DNS record to add only when asked, with a copyable name and value", () => {
    render(row({ kind: "needs_dns" }));

    expect(screen.queryByRole("link")).toBeNull();
    expect(screen.queryByText("CNAME")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show DNS records" }));
    expect(screen.getByText("CNAME")).toBeTruthy();
    expect(screen.getByText("www")).toBeTruthy();
    expect(screen.getByText("acme.ployz.app")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Copy CNAME name" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Copy CNAME value" })).toBeTruthy();
  });
});
