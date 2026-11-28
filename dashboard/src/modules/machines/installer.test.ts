import { expect, it } from "vitest";
import { installLines } from "./installer";

const TOKEN = "pflt_example";
const KEY = "pk1_eyJraW5kIjoicGFzc2tleSJ9";
const hosted = { cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3-beta.4", token: TOKEN };

it("keeps the token off the Shell line, which asks for it, and puts it once in the cloud-init tab", () => {
  const lines = installLines(hosted, KEY);
  expect(lines.shell).not.toContain(TOKEN);
  expect(lines.shell).toMatch(/ --enroll --key pk1_/);
  expect(lines.cloudInit.split(TOKEN)).toHaveLength(2);
});

it("sets PLOYZ_CLOUD_URL only for a self-hosted Cloud", () => {
  expect(installLines(hosted, KEY).shell).not.toContain("PLOYZ_CLOUD_URL");
  expect(installLines({ ...hosted, cloudOrigin: "http://localhost:3000" }, KEY).shell).toBe(`curl -fsSL https://ployz.sh | sudo PLOYZ_CLOUD_URL='http://localhost:3000' sh -s -- 1.2.3-beta.4 --enroll --key ${KEY}`);
});

it("pins the Cloud's own release in every tab, since the stable channel may not speak to this Cloud", () => {
  for (const line of Object.values(installLines(hosted, KEY))) expect(line).toContain(" -- 1.2.3-beta.4");
});

it("keeps the token out of sudo's argv, where ps would show it", () => {
  for (const cloudOrigin of [hosted.cloudOrigin, "http://localhost:3000"]) {
    expect(installLines({ ...hosted, cloudOrigin }, KEY).shell).not.toContain(TOKEN);
  }
});

it("hands the passkey's key to the installer from both tabs, so a cloud-init Server trusts it too", () => {
  const lines = installLines(hosted, KEY);
  expect(lines.shell).toMatch(/ --enroll --key pk1_eyJraW5kIjoicGFzc2tleSJ9$/);
  expect(lines.cloudInit).toMatch(/ sh -s -- 1\.2\.3-beta\.4 --key pk1_eyJraW5kIjoicGFzc2tleSJ9$/);
});
