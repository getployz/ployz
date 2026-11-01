// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { RedeployAction, VariableWarnings } from "./StoreDeploymentPage";

afterEach(cleanup);

it("says copy changes wait for Deploy beside Redeploy, which ships what the Deployment froze", () => {
  const onRedeploy = vi.fn();
  render(<RedeployAction copiesWait disabled={false} onRedeploy={onRedeploy} />);
  expect(screen.getByText("Copies wait for Deploy")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Redeploy" }));
  expect(onRedeploy).toHaveBeenCalledOnce();
});

it("says nothing more beside Redeploy when no copy change is staged", () => {
  render(<RedeployAction copiesWait={false} disabled={false} onRedeploy={() => {}} />);
  expect(screen.getByRole("button", { name: "Redeploy" })).toBeTruthy();
  expect(screen.queryByText("Copies wait for Deploy")).toBeNull();
});

it("shows a sealed reference's whole hint, ways out included, wrapping inside the card", () => {
  render(<VariableWarnings warnings={[{ service: "app", variable: "DBURL", reads: "db.TOK3", sealed_elsewhere: true }]} />);
  const hint = within(screen.getByRole("list", { name: "Warnings" })).getByText(/^DBURL not set in app: it reads db\.TOK3/);
  expect(hint.textContent).toMatch(/Set DBURL here, or make db separate and set TOK3 there, though its volumes start empty$/);
  // The Badge is one line of fixed height by default, which ran the hint past the card and clipped it.
  expect([...hint.classList]).toEqual(expect.arrayContaining(["whitespace-normal", "h-auto"]));
  expect([...hint.classList]).not.toContain("whitespace-nowrap");
  expect([...hint.classList]).not.toContain("h-5");
});
