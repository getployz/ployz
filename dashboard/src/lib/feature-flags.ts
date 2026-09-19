// Build-time flag shared by Dashboard rendering and server authoring actions.
export const variableGroupsEnabled = import.meta.env["VITE_VARIABLE_GROUPS_ENABLED"] === "true";
