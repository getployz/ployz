// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { RedeployAction } from "./StoreDeploymentPage";

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
