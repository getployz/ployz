export type ServiceSettingsSectionId =
  | "source"
  | "networking"
  | "scale"
  | "build"
  | "deploy"
  | "danger";

export type ServiceSettingsSectionMeta = {
  id: ServiceSettingsSectionId;
  label: string;
  description: string;
};

export const SERVICE_SETTINGS_SECTIONS = [
  {
    id: "source",
    description: "Where the code for this service comes from.",
    label: "Source",
  },
  {
    id: "networking",
    description: "How this service is reached, publicly and from other services.",
    label: "Networking",
  },
  {
    id: "scale",
    description: "Replicas and the resources each one gets.",
    label: "Scale",
  },
  {
    id: "build",
    description: "How the image for this service is produced.",
    label: "Build",
  },
  {
    id: "deploy",
    description: "How the container starts, stays healthy, and restarts.",
    label: "Deploy",
  },
  {
    id: "danger",
    description: "Destructive actions. There is no undo.",
    label: "Danger",
  },
] as const satisfies readonly ServiceSettingsSectionMeta[];
