import { expect, it } from "vitest";
import { openStartedDeploymentsChange } from "./open-started-deployments";

it("turns on when you open your running attempt and off when you return to live while it runs", () => {
  expect(openStartedDeploymentsChange({ shownBefore: null, shownNow: "a", deploymentId: "a" })).toBe(true);
  expect(openStartedDeploymentsChange({ shownBefore: "a", shownNow: "a", deploymentId: "a" })).toBeNull();
  expect(openStartedDeploymentsChange({ shownBefore: "a", shownNow: null, deploymentId: null })).toBe(false);
  expect(openStartedDeploymentsChange({ shownBefore: "a", shownNow: "b", deploymentId: "b" })).toBe(true);
  // Switching to another attempt, or yours finishing while shown, leaves it alone.
  expect(openStartedDeploymentsChange({ shownBefore: "a", shownNow: null, deploymentId: "c" })).toBeNull();
  expect(openStartedDeploymentsChange({ shownBefore: null, shownNow: null, deploymentId: null })).toBeNull();
});
