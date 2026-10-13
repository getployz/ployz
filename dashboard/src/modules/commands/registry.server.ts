import "@tanstack/react-start/server-only";
import { orgApprovalApprove, orgApprovalDeny, orgApprovalLs, orgApprovalShow } from "#/modules/approvals/approvals.commands.server";
import { serverUpgradePolicySet, serverUpgradePolicyShow } from "#/modules/server-upgrade/server-upgrade.commands.server";
import { uptimeCheck, uptimeShow } from "#/modules/uptime/uptime.commands.server";

/** Every command Cloud publishes to `ployz` and the sidebar, in manifest order. Publishing one is adding it here. */
export const COMMANDS = [
  serverUpgradePolicyShow,
  serverUpgradePolicySet,
  orgApprovalLs,
  orgApprovalShow,
  orgApprovalApprove,
  orgApprovalDeny,
  uptimeShow,
  uptimeCheck,
] as const;
