# ADR 0034: Host Compatibility Lives In Profiles And Supervisor Adapters

## Status

Accepted.

## Context

Host Runner prepares Linux machines before Ployz services exist. Package names,
Docker installation, service definitions, and service lifecycle commands vary by
distribution. Keeping those differences inline in the bootstrap operation makes
every new host another branch through orchestration code. It also couples release
acceptance to one package-manager command spelling, such as the DNF4
`config-manager --add-repo` syntax that is not accepted by DNF5.

Ployz needs an explicit compatibility matrix whose package, Docker, and
supervisor choices can be tested independently of bootstrap orchestration.

## Decision

Host Runner reads `/etc/os-release` once during its initial read-only host
verification and selects one concrete host platform profile. A profile contains
three independent capabilities:

- package family: Debian, RPM, Arch, Alpine, or SUSE;
- supervisor: systemd or OpenRC;
- Docker installation: Docker's convenience script or one native package/repository strategy.

The compatibility matrix accepts these distribution IDs:

- `arch`, `ubuntu`, `debian`, `raspbian`, `centos`, `fedora`, `rhel`, `ol`,
  `rocky`, `sles`, `opensuse-leap`, `opensuse-tumbleweed`, `almalinux`, `amzn`,
  and `tencentos`;
- `manjaro`, `manjaro-arm`, `endeavouros`, and `cachyos` normalize to Arch;
- `fedora-asahi-remix` normalizes to Fedora;
- `pop`, `linuxmint`, and `zorin` normalize to Ubuntu.

Alpine and postmarketOS are TBD rather than supported hosts. Their APK and
OpenRC adapters exist, but published Ployz Linux artifacts are linked against
glibc and cannot execute on their musl userspace. Supporting these hosts
requires musl-compatible release artifacts and real-host certification; the
presence of host adapters alone does not establish compatibility.

An unrecognized distribution or a missing supervisor directory fails the verify
step before Host Runner writes files or runs host mutation commands.

The Docker strategies are finite and native:

- Alpine and postmarketOS will use APK when musl-compatible release artifacts
  make those hosts supportable;
- Arch-family hosts use pacman against the existing sync databases without
  refreshing them or performing a system upgrade; host system updates and
  required reboots remain operator-owned substrate work;
- Amazon Linux uses DNF's Docker package;
- Rocky downloads Docker's RHEL repository file and installs the Docker CE packages with DNF;
- AlmaLinux and TencentOS download Docker's CentOS repository file and install the Docker CE packages with DNF;
- the remaining accepted profiles use Docker's `get.docker.com` installer.

Repository files are downloaded directly into the package manager's repository
directory. This works with both DNF4 and DNF5 and removes any dependency on a
particular `config-manager` plugin syntax. Every external command uses the Host
Runner's bounded command facility.

Service contracts remain supervisor-neutral. The existing typed NATS and
`ployzd` service specifications render to either a systemd unit or an OpenRC
init script. One supervisor adapter translates install, enable, start, restart,
stop, disable, kill, and active-state operations. OpenRC services use
`supervise-daemon` with restart behavior and explicit network and Docker
dependencies. The control process also reloads NATS through the active host
supervisor.

The adapters are closed enums and direct functions. Ployz does not introduce a
generic package-manager DSL, a trait hierarchy for every host command, or a
third-party service-manager wrapper. A new capability becomes a new explicit
variant; a new distribution normally maps to an existing profile.

## Consequences

The bootstrap plan has one host-verification step and one set of product
operations. OS-specific details stay inside the platform and supervisor
adapters.

Compatibility is optimistic until each profile is exercised on real hosts. The
deterministic suite verifies detection, package selection, Docker strategy,
systemd/OpenRC rendering, lifecycle translation, and fail-before-mutation
behavior. Real-host evidence can tighten version constraints without changing
the orchestration interface.

Changes in upstream package layouts or supervisor behavior require an explicit
matrix or strategy update and focused adapter tests.
