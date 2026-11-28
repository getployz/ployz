import { expect, it } from "vitest";
import { installLines } from "./installer";

const TOKEN = "pmet_example";
const hosted = { cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3-beta.4", token: TOKEN, ingressClosed: false };

it("keeps the token off the Shell line, which asks for it, and puts it once in the cloud-init tab", () => {
  const lines = installLines(hosted, null);
  expect(lines.shell).not.toContain(TOKEN);
  expect(lines.cloudInit.split(TOKEN)).toHaveLength(2);
});

it("prints the line ployz server add --command prints, so one Server joins and waits for its own approval", () => {
  expect(installLines(hosted, "pk1_eyJraW5kIjoicGFzc2tleSJ9").shell).toBe(
    "curl -fsSL https://ployz.sh | sh -s -- 1.2.3-beta.4 && sudo --preserve-env=PLOYZ_ENROLL_TOKEN ployz server add --key pk1_eyJraW5kIjoicGFzc2tleSJ9",
  );
});

it("names a self-hosted Cloud on the line, and only then", () => {
  expect(installLines(hosted, null).shell).not.toContain("--cloud-url");
  expect(installLines({ ...hosted, cloudOrigin: "http://localhost:3000" }, null).shell).toContain(" ployz server add --cloud-url 'http://localhost:3000'");
});

it("pins the Cloud's own release in every tab, since the stable channel may not speak to this Cloud", () => {
  for (const line of Object.values(installLines(hosted, null))) expect(line).toContain(" -- 1.2.3-beta.4 ");
});

it("hands the passkey's key to ployz server add from both tabs, so a cloud-init Server trusts it too", () => {
  for (const line of Object.values(installLines(hosted, "pk1_eyJraW5kIjoicGFzc2tleSJ9"))) {
    expect(line).toMatch(/ ployz server add --key pk1_eyJraW5kIjoicGFzc2tleSJ9$/);
  }
});

it("founds closed only when Cloudflare Tunnel was chosen", () => {
  expect(installLines({ ...hosted, ingressClosed: true }, null).shell).toMatch(/ --ingress-closed$/);
  expect(installLines(hosted, null).shell).not.toContain("--ingress-closed");
});
