import type { ServiceSettingChange } from "@ployz/sdk";
import { settingText } from "#/modules/config-store/store-services";
import { useRuntimeService } from "#/providers/runtime-provider";
import type { StoreService } from "./StoreServiceDrawer";

/** Its deployed value while the next Deploy changes it: what runs now, never what's staged. */
const deployed = (changes: Map<string, ServiceSettingChange>, name: string, current: string) => {
  const change = changes.get(name);
  return change ? settingText(change.before) : current;
};

export function useServiceRuntime({ service, rows, changes }: StoreService, namespace: string | null) {
  const privateDns = deployed(changes, "privateDns", settingText(rows.get("privateDns")?.value) || service.private_dns);
  return useRuntimeService(namespace ? `${namespace}/${privateDns}` : "").runtime;
}
