export type ServiceSettingsSectionId =
  | "template"
  | "source"
  | "networking"
  | "storage"
  | "scale"
  | "build"
  | "deploy"
  | "danger";

export type ServiceSettingsSectionMeta = {
  id: ServiceSettingsSectionId;
  label: string;
};

export const SERVICE_SETTINGS_SECTIONS = [
  { id: "template", label: "Template" },
  { id: "source", label: "Source" },
  { id: "networking", label: "Networking" },
  { id: "storage", label: "Storage" },
  { id: "scale", label: "Scale" },
  { id: "build", label: "Build" },
  { id: "deploy", label: "Deploy" },
  { id: "danger", label: "Danger" },
] as const satisfies readonly ServiceSettingsSectionMeta[];
