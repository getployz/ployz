import { describe, expect, it } from "vitest";
import { prCheck, type PrCheckDestination } from "./pr-check";

const production = (over: Partial<PrCheckDestination> = {}): PrCheckDestination => ({ name: "production", changes: 3, save: null, ...over });
const saved = (over: Partial<NonNullable<PrCheckDestination["save"]>> = {}) => ({ standing: true, changes: 3, ...over });

describe("prCheck", () => {
  it.each<[string, PrCheckDestination[], boolean, string]>([
    ["no Destination", [], true, "No environment deploys main"],
    ["nothing to save", [production({ changes: 0 })], true, "No changes for production"],
    ["not saved", [production()], false, "3 changes to save in Ployz"],
    ["one change, not saved", [production({ changes: 1 })], false, "1 change to save in Ployz"],
    ["changed since saved", [production({ save: saved({ standing: false }) })], false, "Changed since saved · save again"],
    ["saved", [production({ save: saved() })], true, "3 changes go live with this PR"],
    ["one change saved", [production({ changes: 1, save: saved({ changes: 1 }) })], true, "1 change goes live with this PR"],
    ["saved, then nothing left to save", [production({ changes: 0, save: saved({ changes: 2 }) })], true, "2 changes go live with this PR"],
    ["two Destinations, neither saved", [production(), production({ name: "staging-eu", changes: 2 })], false, "5 changes to save in Ployz"],
    ["two Destinations, one saved", [production({ save: saved() }), production({ name: "staging-eu", changes: 2 })], false, "2 changes to save in Ployz"],
    ["two Destinations, both saved", [production({ save: saved() }), production({ name: "staging-eu", save: saved({ changes: 2 }) })], true,
      "5 changes go live with this PR"],
    ["two Destinations, one with nothing to save", [production({ save: saved() }), production({ name: "staging-eu", changes: 0 })], true,
      "3 changes go live with this PR"],
  ])("%s", (_, destinations, passing, reason) => {
    expect(prCheck(destinations, "main")).toEqual({ passing, reason });
  });
});
