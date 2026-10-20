import { expect, it } from "vitest";
import { installLines } from "./install-lines";

const TOKEN = "pflt_example";
const hosted = { cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3-beta.4", token: TOKEN };

it("names every server in the Terraform snippet, since hcloud_server requires a name", () => {
  const { terraform } = installLines(hosted);
  const resource = terraform.slice(terraform.indexOf('resource "hcloud_server"'));
  expect(resource).toMatch(/^\s+name\s+= "web-\$\{count\.index \+ 1\}"$/m);
});

it("keeps the token off the Shell line, which asks for it, and puts it once in each user-data tab", () => {
  const lines = installLines(hosted);
  expect(lines.shell).not.toContain(TOKEN);
  expect(lines.shell).toMatch(/ --enroll$/);
  for (const text of [lines.cloudInit, lines.terraform]) {
    expect(text.split(TOKEN)).toHaveLength(2);
  }
  expect(lines.terraform).toContain("PLOYZ_FLEET_TOKEN=${var.ployz_fleet_token}");
});

it("sets PLOYZ_CLOUD_URL only for a self-hosted Cloud", () => {
  expect(installLines(hosted).shell).not.toContain("PLOYZ_CLOUD_URL");
  expect(installLines({ ...hosted, cloudOrigin: "http://localhost:3000" }).shell).toBe("curl -fsSL https://ployz.sh | sudo PLOYZ_CLOUD_URL='http://localhost:3000' sh -s -- 1.2.3-beta.4 --enroll");
});

it("pins the Cloud's own release in every tab, since the stable channel may not speak to this Cloud", () => {
  const lines = installLines(hosted);
  expect(lines.shell).toBe("curl -fsSL https://ployz.sh | sudo sh -s -- 1.2.3-beta.4 --enroll");
  expect(lines.cloudInit).toContain("PLOYZ_FLEET_TOKEN=pflt_example sh -s -- 1.2.3-beta.4");
  expect(lines.terraform).toContain("PLOYZ_FLEET_TOKEN=${var.ployz_fleet_token} sh -s -- 1.2.3-beta.4");
});

it("keeps the token out of sudo's argv, where ps would show it", () => {
  for (const cloudOrigin of [hosted.cloudOrigin, "http://localhost:3000"]) {
    expect(installLines({ ...hosted, cloudOrigin }).shell).not.toContain(TOKEN);
  }
});
