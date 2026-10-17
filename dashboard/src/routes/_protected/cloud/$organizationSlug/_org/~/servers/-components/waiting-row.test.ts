import { expect, it } from "vitest";
import { portText } from "./waiting-row";

it("says why a port is unknown instead of a bare Unknown", () => {
  expect(portText({ state: "unknown", reason: "no_answer" })).toBe("No answer · firewall dropping it?");
  expect(portText({ state: "unknown", reason: "port_busy" })).toBe("Couldn't listen · port in use");
  expect(portText({ state: "open" })).toBe("Open");
  expect(portText({ state: "closed" })).toBe("Closed");
});
