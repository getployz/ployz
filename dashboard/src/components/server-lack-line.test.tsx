// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { LackLine } from "./server-lack-line";

afterEach(cleanup);

it("offers Upgrade only when the Server is behind the channel's release and may upgrade", () => {
  const upgrade = vi.fn();
  render(<LackLine server="web-2" onUpgrade={upgrade} remedy={{ kind: "behind", sentence: "Logs need a newer Ployz on web-2. 0.2.4 is out.", canUpgrade: true }} />);
  expect(screen.getByRole("status").textContent).toBe("Logs need a newer Ployz on web-2. 0.2.4 is out.Upgrade");
  fireEvent.click(screen.getByRole("button", { name: "Upgrade" }));
  expect(upgrade).toHaveBeenCalledOnce();
  cleanup();
  render(<LackLine server="web-2" onUpgrade={upgrade} remedy={{ kind: "behind", sentence: "Logs need a newer Ployz on web-2. 0.2.4 is out.", canUpgrade: false }} />);
  expect(screen.queryByRole("button")).toBeNull();
});

it("says the feature is missing from the Server's release with no button", () => {
  render(<LackLine server="web-2" onUpgrade={() => {}} remedy={{ kind: "lack", sentence: "Volume move is not in Ployz 0.2.4 on web-2 yet." }} />);
  expect(screen.getByRole("status").textContent).toBe("Volume move is not in Ployz 0.2.4 on web-2 yet.");
  expect(screen.queryByRole("button")).toBeNull();
});

it("follows an Upgrade under way, waiting for the Server, or failed", () => {
  const retry = vi.fn();
  const { rerender } = render(<LackLine server="web-2" onUpgrade={retry} remedy={{ kind: "upgrading", target: "0.2.4" }} />);
  expect(screen.getByRole("status").textContent).toBe("Upgrading web-2 to 0.2.4");
  expect(screen.queryByRole("button")).toBeNull();
  rerender(<LackLine server="web-2" onUpgrade={retry} remedy={{ kind: "when-back", release: "0.2.4" }} />);
  expect(screen.getByRole("status").textContent).toBe("web-2 upgrades to 0.2.4 when it’s back");
  rerender(<LackLine server="web-2" onUpgrade={retry} remedy={{ kind: "failed", target: "0.2.4", nothingChanged: true, details: null, canRetry: true }} />);
  expect(screen.getByRole("status").textContent).toBe("0.2.4 didn’t install on web-2, and we’re on it.Try again");
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  expect(retry).toHaveBeenCalledOnce();
});
