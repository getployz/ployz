# Compose Is A Deploy Input Adapter, Not The Core Model

Ployz may adopt Docker Compose terminology in core language where it matches Ployz semantics, including services, replicas, volumes, configs, secrets, healthchecks, ports, and update order. Compose itself remains a deploy input adapter: a Compose project maps into one Ployz namespace, but Compose project structure, networks, lifecycle conditions, and adapter extensions do not define the core orchestration model.

Compose translation fails closed by default when input semantics are unsupported; a caller must explicitly opt into lossy translation with `--allow-unsupported`.
