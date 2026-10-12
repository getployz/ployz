import { describe, expect, it } from "vitest";
import { strictValidator } from "#/server/tanstack";
import { AddOffsiteInput } from "./volume-offsite.functions";

const SECRET = "hunter2-offsite-secret";
const typed = { kind: "new", endpoint: "s3.eu-central-1.amazonaws.com", bucket: "acme-offsite", accessKey: "AKIAEXAMPLE", secretKey: SECRET };
const validate = (choice: Record<string, string | boolean>) => strictValidator(AddOffsiteInput)["~standard"].validate({
  organizationSlug: "acme", environment: { project: "shop", environment: "production" }, volumeId: "vol-1", choice,
});

describe("Add offsite input", () => {
  it("never echoes the secret key in a refusal the browser reads", async () => {
    for (const choice of [{ ...typed, bucket: "Not A Bucket" }, { ...typed, extra: true }, { ...typed, kind: "other" }]) {
      const result = await validate(choice);
      expect(result).toHaveProperty("issues");
      expect(JSON.stringify(result)).not.toContain(SECRET);
    }
  });
});
