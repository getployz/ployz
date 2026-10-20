import { expect, it } from "vitest";
import { installLines } from "./install-lines";

const TOKEN = "pflt_example";
const hosted = { cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3-beta.4", token: TOKEN };

it("names every server in the Terraform snippet, since hcloud_server requires a name", () => {
  const { terraform } = installLines(hosted);
  const resource = terraform.slice(terraform.indexOf('resource "hcloud_server"'));
  expect(resource).toMatch(/^\s+name\s+= "web-\$\{count\.index \+ 1\}"$/m);
});

it("puts the token in every tab exactly once and keeps it out of the Terraform user_data", () => {
  const lines = installLines(hosted);
  for (const text of [lines.shell, lines.cloudInit, lines.terraform]) {
    expect(text.split(TOKEN)).toHaveLength(2);
  }
  expect(lines.terraform).toContain("PLOYZ_FLEET_TOKEN=${var.ployz_fleet_token}");
});

it("sets PLOYZ_CLOUD_URL only for a self-hosted Cloud", () => {
  expect(installLines(hosted).shell).not.toContain("PLOYZ_CLOUD_URL");
  expect(installLines({ ...hosted, cloudOrigin: "http://localhost:3000" }).shell).toContain("PLOYZ_CLOUD_URL='http://localhost:3000' ");
});

it("pins the Cloud's own release in every tab, since the stable channel may not speak to this Cloud", () => {
  const lines = installLines(hosted);
  expect(lines.shell).toBe(
    "curl -fsSL https://ployz.sh | PLOYZ_FLEET_TOKEN=pflt_example sudo --preserve-env=PLOYZ_FLEET_TOKEN sh -s -- 1.2.3-beta.4",
  );
  expect(lines.cloudInit).toContain("PLOYZ_FLEET_TOKEN=pflt_example sh -s -- 1.2.3-beta.4");
  expect(lines.terraform).toContain("PLOYZ_FLEET_TOKEN=${var.ployz_fleet_token} sh -s -- 1.2.3-beta.4");
});

it("keeps the token out of sudo's argv, where ps would show it", () => {
  for (const cloudOrigin of [hosted.cloudOrigin, "http://localhost:3000"]) {
    const [beforeSudo, afterSudo] = installLines({ ...hosted, cloudOrigin }).shell.split(" sudo ");
    expect(beforeSudo).toMatch(/ PLOYZ_FLEET_TOKEN=pflt_example$/);
    expect(afterSudo).not.toContain(TOKEN);
  }
});
