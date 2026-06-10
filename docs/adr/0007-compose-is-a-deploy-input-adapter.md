# Compose Is A Deploy Input Adapter, Not The Core Model

Ployz may adopt Docker Compose terminology in core language where it matches Ployz semantics, including services, replicas, volumes, configs, secrets, healthchecks, ports, and update order. Compose itself remains a deploy input adapter: a Compose project maps into one Ployz namespace, but Compose project structure, networks, lifecycle conditions, and adapter extensions do not define the core orchestration model.

This deliberately trades perfect Compose compatibility for a smaller Ployz domain model. The adapter can publish a support matrix and translate familiar Compose inputs without making Compose the source of runtime truth.
