import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

export default defineConfig({
  integrations: [
    starlight({
      title: "Ployz",
      logo: {
        src: "./public/assets/operator-loop.svg",
        alt: "Ployz operator loop",
      },
      customCss: ["./src/styles/custom.css"],
      pagefind: false,
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/getployz/ployz",
        },
      ],
      sidebar: [
        {
          label: "Get Started",
          items: [
            { label: "Overview", slug: "start/overview" },
            { label: "Install", slug: "start/install" },
            { label: "Daemon Runtime", slug: "start/daemon-runtime" },
            { label: "First Operation", slug: "start/first-operation" },
          ],
        },
        {
          label: "Concepts",
          items: [
            { label: "Operator Loop", slug: "concepts/operator-loop" },
            { label: "Primitives, Not Policies", slug: "concepts/primitives-not-policies" },
            { label: "State Model", slug: "concepts/state-model" },
            { label: "Daemon and Data Plane", slug: "concepts/daemon-data-plane" },
            { label: "ZFS Storage", slug: "concepts/zfs-storage" },
          ],
        },
        {
          label: "Commands",
          items: [
            { label: "Command Model", slug: "commands/overview" },
            { label: "Current Commands", slug: "commands/current" },
            { label: "North-Star Primitives", slug: "commands/north-star" },
          ],
        },
        {
          label: "Guides",
          items: [
            { label: "Guide Index", slug: "guides/overview" },
            { label: "Deploy a Service", slug: "guides/deploy-service" },
            { label: "Add a Machine", slug: "guides/add-machine" },
            { label: "Move a Workload", slug: "guides/move-workload" },
            { label: "Branch an Environment", slug: "guides/branch-environment" },
            { label: "Promote and Roll Back", slug: "guides/promote-rollback" },
          ],
        },
        {
          label: "Security",
          items: [
            { label: "Trust Model", slug: "security/trust-model" },
            { label: "Store Secrets", slug: "security/store-secrets" },
          ],
        },
        {
          label: "Agents",
          items: [
            { label: "Agent Contract", slug: "agents/contract" },
            { label: "Operation Loop", slug: "agents/operation-loop" },
            { label: "LLM Resources", slug: "agents/llm-resources" },
          ],
        },
        {
          label: "Reference",
          items: [
            { label: "Reference Index", slug: "reference/overview" },
            { label: "Runtime Modes", slug: "reference/runtime-modes" },
            { label: "Config and Paths", slug: "reference/config" },
            { label: "Repo Sources", slug: "reference/repo-sources" },
          ],
        },
      ],
    }),
  ],
});
