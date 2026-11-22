import type { ConfigCommand, EnvironmentRef } from "@ployz/sdk";

type AdmitDeploy = Extract<ConfigCommand, { command: "admit"; admit: "deploy" }>;

/**
 * The write that admits Deployment `id` of `environment`: every Service unless `services` narrows it, at the Working
 * State unless `version` pins one. `message` is sent only when given, `null` included.
 */
export function admitDeploy(environment: EnvironmentRef, id: string, options: {
  services?: readonly string[];
  version?: string | null;
  acceptVolumeLoss?: readonly string[];
  message?: string | null;
} = {}): AdmitDeploy {
  const command: AdmitDeploy = {
    command: "admit", admit: "deploy", id, environment,
    services: [...options.services ?? []], version: options.version ?? null, accept_volume_loss: [...options.acceptVolumeLoss ?? []],
  };
  if (options.message !== undefined) command.message = options.message;
  return command;
}
