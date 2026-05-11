---
title: Ployz Open Core
description: Documentation for the Ployz CLI, daemon, and self-hosted runtime.
template: splash
hero:
  title: Run small clusters with explicit commands.
  tagline: "Ployz turns infrastructure work into foreground operations: add machines, deploy workloads, move state, branch environments, promote, roll back, and verify what happened."
  image:
    file: ../../assets/operator-loop.svg
  actions:
    - text: Start here
      link: /start/overview/
      icon: right-arrow
    - text: Current commands
      link: /commands/current/
      variant: secondary
llms:
  summary: Product entrypoint and scope boundary.
---

Ployz is the open-core orchestration core for small clusters. These docs cover
`ployzctl`, `ployzd`, local development, agents, and self-hosted machines.

Ployz Cloud has separate docs. Dashboard workflows, billing, teams, hosted
machine pools, and managed web UI behavior stay out of this site.

## The docs shape

<div class="operator-grid">

- **Get Started**  
  Install the daemon runtime, inspect the local surface, and run a first safe
  operation.

- **Concepts**  
  Understand the operator loop, state model, daemon boundary, NATS, and ZFS.

- **Commands**  
  Separate current executable commands from north-star primitive names.

- **Guides**  
  Task-shaped operator recipes with verification and recovery paths.

- **Security**  
  Make trust boundaries and store authority explicit.

- **Agents**  
  Use Ployz from coding agents without guessing from display text.

</div>
