import { expect, it } from "vitest";
import { toErrorMessage } from "./error-message";

it("says Cloud couldn't be reached, not the browser's words, when a request never got an answer", () => {
  const message = toErrorMessage(new TypeError("Failed to fetch"), "Couldn't make the invite.");
  expect(message).not.toContain("Failed to fetch");
  expect(message).not.toBe("Couldn't make the invite.");
});

it("shows Cloud's own words for its error, and the fallback for anything else", () => {
  expect(toErrorMessage({ _tag: "PublicError", code: "CONFLICT", message: "That name is taken." }, "Couldn't rename.")).toBe("That name is taken.");
  expect(toErrorMessage("soak injected", "Couldn't rename.")).toBe("Couldn't rename.");
});
