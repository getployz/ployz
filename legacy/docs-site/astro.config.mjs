import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import rehypeMermaid from "rehype-mermaid";

export default defineConfig({
  site: "https://docs.ployz.org",
  markdown: {
    rehypePlugins: [
      [
        rehypeMermaid,
        {
          strategy: "inline-svg",
          mermaidConfig: { theme: "neutral" },
        },
      ],
    ],
  },
  integrations: [
    starlight({
      title: "Ployz",
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
            { label: "Introduction", slug: "introduction" },
            { label: "Quickstart", slug: "quickstart" },
            { label: "Installation", slug: "installation" },
          ],
        },
        {
          label: "Core Concepts",
          items: [
            { label: "Primitives, Not Policies", slug: "concepts/primitives" },
            { label: "Cluster Model", slug: "concepts/cluster-model" },
            { label: "Data Plane", slug: "concepts/data-plane" },
            { label: "Storage", slug: "concepts/storage" },
          ],
        },
        {
          label: "Operations",
          items: [
            { label: "Machines", slug: "operations/machines" },
            { label: "Deploy", slug: "operations/deploy" },
            { label: "Branch and Promote", slug: "operations/branch-and-promote" },
            { label: "Migrate", slug: "operations/migrate" },
            { label: "Rollback", slug: "operations/rollback" },
          ],
        },
        {
          label: "Configuration",
          items: [
            { label: "Daemon", slug: "configuration/daemon" },
            { label: "Networking", slug: "configuration/networking" },
            { label: "Runtimes", slug: "configuration/runtimes" },
          ],
        },
        {
          label: "Architecture",
          items: [
            { label: "Overview", slug: "architecture/overview" },
            { label: "Coordination", slug: "architecture/coordination" },
            { label: "Routing", slug: "architecture/routing" },
          ],
        },
        {
          label: "CLI Reference",
          items: [
            { label: "Current Commands", slug: "commands/current" },
            { label: "Overview", slug: "cli/overview" },
            { label: "Machine", slug: "cli/machine" },
            { label: "Deploy", slug: "cli/deploy" },
            { label: "Branch", slug: "cli/branch" },
            { label: "Migrate", slug: "cli/migrate" },
            { label: "Mesh", slug: "cli/mesh" },
            { label: "Image", slug: "cli/image" },
            { label: "Build", slug: "cli/build" },
          ],
        }
      ],
    }),
  ],
});
