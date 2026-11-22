import { expect, it } from "vitest";
import { installLines } from "./installer";

const TOKEN = "pflt_example";
const hosted = { cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3-beta.4", token: TOKEN };

it("keeps the token off the Shell line, which asks for it, and puts it once in the cloud-init tab", () => {
  const lines = installLines(hosted);
  expect(lines.shell).not.toContain(TOKEN);
  expect(lines.shell).toMatch(/ --enroll$/);
  expect(lines.cloudInit.split(TOKEN)).toHaveLength(2);
});

it("sets PLOYZ_CLOUD_URL only for a self-hosted Cloud", () => {
  expect(installLines(hosted).shell).not.toContain("PLOYZ_CLOUD_URL");
  expect(installLines({ ...hosted, cloudOrigin: "http://localhost:3000" }).shell).toBe("curl -fsSL https://ployz.sh | sudo PLOYZ_CLOUD_URL='http://localhost:3000' sh -s -- 1.2.3-beta.4 --enroll");
});

it("pins the Cloud's own release in every tab, since the stable channel may not speak to this Cloud", () => {
  for (const line of Object.values(installLines(hosted))) expect(line).toContain(" -- 1.2.3-beta.4");
});

it("keeps the token out of sudo's argv, where ps would show it", () => {
  for (const cloudOrigin of [hosted.cloudOrigin, "http://localhost:3000"]) {
    expect(installLines({ ...hosted, cloudOrigin }).shell).not.toContain(TOKEN);
  }
});
