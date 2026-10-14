export default {
  $schema: "https://react.doctor/schema/config.json",
  ignore: {
    files: [".output/**"],
    overrides: [
      {
        files: [
          "src/routes/_protected/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRegistryCredentialForm.tsx",
        ],
        rules: ["react-doctor/no-prevent-default"],
      },
      {
        files: ["src/components/environment-breadcrumbs.tsx"],
        rules: ["react-doctor/no-prevent-default"],
      },
      {
        files: [
          "src/lib/errors.ts",
          "src/lib/result-http.ts",
          "src/lib/result.ts",
        ],
        rules: ["deslop/unused-export"],
      },
    ],
  },
};
