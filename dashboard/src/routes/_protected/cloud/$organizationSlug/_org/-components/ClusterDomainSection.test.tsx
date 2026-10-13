// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ClusterDomainRow } from "#/modules/cluster-domain/cluster-domain";
import { ClusterDomainSection } from "./ClusterDomainSection";

const ready: Omit<ClusterDomainRow, "id"> = {
  name: "acme.ployz.app",
  recordsSyncedAt: new Date(Date.now() - 5 * 60_000),
  traffic: { kind: "probed", unreachable: [] },
  certificateNotAfter: new Date(Date.now() + 60 * 86_400_000),
  checkedAt: new Date(Date.now() - 5 * 60_000),
};

describe("ClusterDomainSection", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it("says when the domain arrives before the first deploy", () => {
    render(<ClusterDomainSection organizationSlug="acme" domain={null} onCheck={vi.fn()} />);

    expect(screen.getByText("You’ll get one on your first deploy.")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("shows a ready domain with nothing to act on", () => {
    render(<ClusterDomainSection organizationSlug="acme" domain={ready} onCheck={vi.fn()} />);

    expect(screen.getByText("acme.ployz.app")).toBeTruthy();
    expect(screen.getByText("Ready")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Check again" })).toBeNull();
  });

  it("checks again until the sync records a newer check", async () => {
    const blocked = { ...ready, traffic: { kind: "probed", unreachable: [{ machineId: "a", address: "203.0.113.1" }] } } as Omit<ClusterDomainRow, "id">;
    const onCheck = vi.fn(() => Promise.resolve());
    const { rerender } = render(<ClusterDomainSection organizationSlug="acme" domain={blocked} onCheck={onCheck} />);

    expect(screen.getByText("Needs attention")).toBeTruthy();
    expect(screen.getByText("Traffic can’t reach 203.0.113.1. Make sure port 80 is open.")).toBeTruthy();
    const button = () => screen.getByText("Check again").closest("button") as HTMLButtonElement;
    await act(async () => { fireEvent.click(button()); });
    expect(onCheck).toHaveBeenCalledOnce();
    expect(button().disabled).toBe(true);

    rerender(<ClusterDomainSection organizationSlug="acme" domain={{ ...blocked, checkedAt: new Date() }} onCheck={onCheck} />);
    expect(button().disabled).toBe(false);
  });

  it("says when something else answers on port 80", () => {
    const taken = { ...ready, traffic: { kind: "probed", unreachable: [], taken: [{ machineId: "a", address: "203.0.113.1" }] } } as Omit<ClusterDomainRow, "id">;
    render(<ClusterDomainSection organizationSlug="acme" domain={taken} onCheck={vi.fn()} />);

    expect(screen.getByText("Needs attention")).toBeTruthy();
    expect(screen.getByText("Something else answers on port 80 of 203.0.113.1. Free the port for Ployz.")).toBeTruthy();
    expect(screen.getByText("Check again")).toBeTruthy();
  });

  it("says no Server receives traffic rather than asking for a new one", async () => {
    const domain = { ...ready, traffic: { kind: "no_servers" } } as const;
    const component = () => <ClusterDomainSection organizationSlug="acme" domain={domain} onCheck={vi.fn()} />;
    render(<RouterProvider router={createRouter({ routeTree: createRootRoute({ component }), history: createMemoryHistory() })} />);

    expect(await screen.findByText("No server receives traffic.")).toBeTruthy();
    expect(screen.getByText("Needs attention")).toBeTruthy();
  });

  it("stays ready when one Server answers, and names the ones traffic skips", () => {
    const degraded = { ...ready, traffic: {
      kind: "probed",
      reachable: [{ machineId: "a", address: "203.0.113.1" }],
      unreachable: [{ machineId: "b", address: "203.0.113.2" }],
      taken: [{ machineId: "c", address: "203.0.113.3" }],
    } } as Omit<ClusterDomainRow, "id">;
    render(<ClusterDomainSection organizationSlug="acme" domain={degraded} onCheck={vi.fn()} />);

    expect(screen.getByText("Ready")).toBeTruthy();
    expect(screen.getByText("Port 80 closed on 203.0.113.2. Port 80 taken on 203.0.113.3.")).toBeTruthy();
    expect(screen.getByText("Check again")).toBeTruthy();
  });
});
