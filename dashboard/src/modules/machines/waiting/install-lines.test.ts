import { expect, it } from "vitest";
import { installLines } from "./install-lines";

const TOKEN = "pflt_example";

it("names every server in the Terraform snippet, since hcloud_server requires a name", () => {
  const { terraform } = installLines("https://ployz.dev", TOKEN);
  const resource = terraform.slice(terraform.indexOf('resource "hcloud_server"'));
  expect(resource).toMatch(/^\s+name\s+= "web-\$\{count\.index \+ 1\}"$/m);
});

it("puts the token in every tab exactly once and keeps it out of the Terraform user_data", () => {
  const lines = installLines("https://ployz.dev", TOKEN);
  for (const text of [lines.shell, lines.cloudInit, lines.terraform]) {
    expect(text.split(TOKEN)).toHaveLength(2);
  }
  expect(lines.terraform).toContain("PLOYZ_ENROLL_TOKEN=${var.ployz_enroll_token}");
});

it("sets PLOYZ_CLOUD_URL only for a self-hosted Cloud", () => {
  expect(installLines("https://ployz.dev", TOKEN).shell).not.toContain("PLOYZ_CLOUD_URL");
  expect(installLines("http://localhost:3000", TOKEN).shell).toContain("PLOYZ_CLOUD_URL='http://localhost:3000' ");
});
