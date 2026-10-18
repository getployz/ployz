import type { FleetTokenLifetime } from "#/modules/machines/waiting/fleet-token";

const DEFAULT_CLOUD_ORIGIN = "https://ployz.dev";

export type InstallLines = { readonly shell: string; readonly cloudInit: string; readonly terraform: string };

export function installLines(cloudOrigin: string, token: string): InstallLines {
  const cloud = cloudOrigin === DEFAULT_CLOUD_ORIGIN ? "" : `PLOYZ_CLOUD_URL='${cloudOrigin}' `;
  const run = (secret: string) => `curl -fsSL https://ployz.sh | ${cloud}PLOYZ_ENROLL_TOKEN=${secret} sh`;
  return {
    shell: `curl -fsSL https://ployz.sh | sudo ${cloud}PLOYZ_ENROLL_TOKEN=${token} sh`,
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
