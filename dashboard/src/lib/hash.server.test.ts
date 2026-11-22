import { expect, it } from "vitest";
import { sha256Hex } from "#/lib/hash.server";

// Stored token and claim-secret hashes, and pairing generations Machines carry as cluster keys, must never change.
it("hashes a secret to the hex digest already stored", () => {
  expect(sha256Hex("ployz_example-secret")).toBe("824c25794ed068e10b8d373c37c825451b45fefc6cc2e96f150192d044db688d");
  expect(sha256Hex("")).toBe("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
});
