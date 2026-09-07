export default {
  $schema: "https://react.doctor/schema/config.json",
  ignore: {
    files: [".output/**"],
    overrides: [
      // TanStack Form integration in this repo intentionally submits through
      // event.preventDefault(); form.handleSubmit(); (see the local
      // ployz-tanstack skill). These forms also depend on client-side mutation
      // state and post-submit navigation/refetch behavior.
      {
        files: [
          "src/routes/_protected/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRegistryCredentialForm.tsx",
        ],
        rules: ["react-doctor/no-prevent-default"],
      },
      // Client-only app-shell dialog: creating an environment requires the
      // mounted router/query client so it can invalidate and navigate to the
      // created environment inside the current project switcher.
      {
        files: ["src/components/navigation-switcher.tsx"],
        rules: ["react-doctor/no-prevent-default"],
      },
      // Public app error/result surfaces documented in AGENTS.md. Keeping these
      // exports stable is more accurate than fake-importing them.
      {
        files: [
          "src/lib/errors.ts",
          "src/lib/result-http.ts",
          "src/lib/result.ts",
        ],
        rules: ["deslop/unused-export"],
      },
      // Tailwind v4 loads this module by string from src/styles.css via @plugin.
      {
        files: ["src/tailwind-typography.js"],
        rules: ["deslop/unused-export"],
      },
    ],
  },
};
