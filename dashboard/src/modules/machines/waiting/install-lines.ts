import type { FleetTokenLifetime } from "#/modules/machines/waiting/fleet-token";

const DEFAULT_CLOUD_ORIGIN = "https://ployz.dev";

export type InstallLines = { readonly shell: string; readonly cloudInit: string; readonly terraform: string };

export type InstallTarget = { readonly cloudOrigin: string; readonly ployzVersion: string; readonly token: string };

/**
 * Every line pins the release this Cloud's SDK speaks, since the stable channel may be older or newer. The Shell line
 * carries no token: install.sh asks for it on the terminal, so it stays out of shell history and `ps`. cloud-init and
 * Terraform have no terminal, so they pass it in the environment.
 */
export function installLines({ cloudOrigin, ployzVersion, token }: InstallTarget): InstallLines {
  const cloud = cloudOrigin === DEFAULT_CLOUD_ORIGIN ? "" : `PLOYZ_CLOUD_URL='${cloudOrigin}' `;
  const run = (secret: string) => `curl -fsSL https://ployz.sh | ${cloud}PLOYZ_ENROLL_TOKEN=${secret} sh -s -- ${ployzVersion}`;
  return {
    shell: `curl -fsSL https://ployz.sh | sudo ${cloud}sh -s -- ${ployzVersion} --enroll`,
    cloudInit: `#cloud-config\nruncmd:\n  - ${run(token)}`,
    terraform: [
      `variable "ployz_enroll_token" {`,
      `  type      = string`,
      `  sensitive = true`,
      `  default   = "${token}"`,
      `}`,
      ``,
      `resource "hcloud_server" "web" {`,
      `  count       = 3`,
      "  name        = \"web-${count.index + 1}\"",
      `  server_type = "cpx41"`,
      `  image       = "ubuntu-24.04"`,
      `  user_data   = <<-EOT`,
      `    #cloud-config`,
      `    runcmd:`,
      `      - ${run("${var.ployz_enroll_token}")}`,
      `  EOT`,
      `}`,
    ].join("\n"),
  };
}

export function cliLifetimeLabel(expiresIn: FleetTokenLifetime): string {
  return expiresIn === "1d" ? "24h" : expiresIn;
}
