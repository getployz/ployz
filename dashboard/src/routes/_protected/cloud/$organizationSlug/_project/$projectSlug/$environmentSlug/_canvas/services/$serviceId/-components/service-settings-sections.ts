export const SERVICE_SETTINGS_SECTIONS = [
  { id: "source", label: "Source" },
  { id: "networking", label: "Networking" },
  { id: "storage", label: "Storage" },
  { id: "scale", label: "Scale" },
  { id: "build", label: "Build" },
  { id: "deploy", label: "Deploy" },
  { id: "danger", label: "Danger" },
] as const;

export type ServiceSettingsSectionId = (typeof SERVICE_SETTINGS_SECTIONS)[number]["id"];
