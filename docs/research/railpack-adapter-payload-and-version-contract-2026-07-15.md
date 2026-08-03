# Railpack adapter payload and version contract

Research for [Pin the Railpack adapter payload and version contract](https://github.com/getployz/ployz/issues/519), refreshed 2026-07-15 against the authoritative latest releases: Railpack `v0.31.0` (`ebb3c1ccbd6ca2386711d21197d74681d33b4df7`) and BuildKit `v0.31.1`. This note separates upstream evidence from the Ployz contract recommendation.

## Frozen recommendation

```rust
pub enum BuildAdapter {
    Dockerfile {
        dockerfile: RelativePath,
        target: Option<StageName>,
    },
    Railpack {
        cache_scope: BuildCacheScope,
    },
}
```

`BuildCacheScope` is a typed, opaque, non-secret, stable identifier minted by the driver for one project/build-cache trust boundary. It is the only Railpack-specific request field at beta. It is required, bounded in length, safe for evidence, and must not contain a repository URL, credential, branch, commit, or human project name. Ployz derives the frontend argument as a versioned encoding such as `ployz-railpack-v1-<base32(cache_scope)>` and never accepts a raw `cache-key` string.

Do **not** add provider, Railpack version, frontend reference, or BuildKit version to `BuildAdapter::Railpack`:

- Provider choice is source configuration. Railpack autodetects it unless the source's `railpack.json` selects `provider`; `prepare` exposes no provider flag ([configuration reference](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/docs/src/content/docs/config/file.mdx#L140-L162), [CLI plan flags](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/cli/common.go#L16-L48)).
- Tool versions are executor mechanism, not caller policy. Put them in one Ployz release-level `BuildToolchain` constant/configuration, emit the selected tuple as operation evidence, and change it only with a Ployz release plus compatibility acceptance.
- `cache_scope` cannot be derived safely from `git_url + subdir`: the core has no Cloud tenant/project authority, and two callers can legitimately build the same public URL while requiring separate writable mount caches. The opaque scope preserves Cloud's ownership of project identity while giving core a narrow isolation primitive.

The beta toolchain tuple should initially be:

```text
Railpack release:       v0.31.0
Railpack CLI amd64:     sha256:f75416cf4c452db2841d864f54dbfd8e4d77f2d4a02b23b87561e7760fa278fd
Railpack CLI arm64:     sha256:de4c197e3a9d0c3de14d1e55fe933611622b399f35f495b4274012609490158a
Railpack frontend:      ghcr.io/railwayapp/railpack-frontend@sha256:6a957cddcc3ccf0f4fe8980d94fa99879e00522aabb63c4e88b33f284acaea09
BuildKit daemon image:  moby/buildkit@sha256:6b59b7df63a8cb9902736f9ddf7fcff8261613d3e7449b8ea8b7537fc399c03a (v0.31.1)
```

The digest observations above are the multi-platform index digests returned by the upstream registries on 2026-07-15. Retain the human tags only as evidence; execution must use the digests. OCI descriptors define the digest as the content identifier and require consumers to verify fetched content against it ([OCI descriptor specification](https://github.com/opencontainers/image-spec/blob/v1.1.1/descriptor.md#digests)).

## Upstream evidence

### CLI and frontend form one Railpack version

Railpack's production guide says the CLI analyzes source and generates a plan, while the custom frontend consumes that plan, and explicitly recommends using the same frontend version that generated it ([production guide](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/docs/src/content/docs/guides/running-railpack-in-production.mdx#L9-L68)). The release process creates one tag, builds platform CLI binaries, and publishes the frontend image from that release; the frontend image copies the freshly compiled `railpack` binary and starts it as `railpack frontend` ([release definition](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/.goreleaser.yml#L4-L70), [frontend image](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/images/alpine/frontend/goreleaser.Dockerfile#L1-L7)). Release `v0.31.0` publishes checksum-bearing Linux assets for both `x86_64-unknown-linux-musl` and `arm64-unknown-linux-musl` ([release](https://github.com/railwayapp/railpack/releases/tag/v0.31.0), [checksums](https://github.com/railwayapp/railpack/releases/download/v0.31.0/checksums.txt)).

**Ployz consequence:** CLI and frontend equality is exact, not a compatible range: the CLI asset and frontend image must come from the same Railpack release tag/commit. A build receipt should record that release and the actual asset/image digests.

### BuildKit has no published three-way compatibility formula

Railpack `v0.31.0` compiles its BuildKit client/frontend code against `github.com/moby/buildkit v0.31.1`, which is also the current official BuildKit release ([Railpack `go.mod`](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/go.mod#L16-L19), [BuildKit release](https://github.com/moby/buildkit/releases/tag/v0.31.1)). Railpack's production documentation requires BuildKit and documents `gateway.v0`, but it states no supported daemon version range and no formula relating Railpack versions to `moby/buildkit` tags ([production guide](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/docs/src/content/docs/guides/running-railpack-in-production.mdx#L39-L68)). BuildKit instead promises backward compatibility for its gRPC API in both client/server directions, says the LLB API is among the surfaces it does not plan to break, and uses feature detection; it also supports only the current feature release and offers no LTS line ([BuildKit API policy](https://github.com/moby/buildkit/blob/v0.31.1/PROJECT.md#api-stability), [release support](https://github.com/moby/buildkit/blob/v0.31.1/PROJECT.md#releases)).

**Ployz consequence:** do not invent semver arithmetic. Pin BuildKit independently as part of the same tested Ployz toolchain tuple, preferring the current supported BuildKit feature release; at this snapshot Railpack's dependency and that release happen to coincide at `v0.31.1`, but equality is not an enduring upstream rule. An upgrade is atomic: choose one exact Railpack release, its same-release frontend digest, and one exact current BuildKit image digest; pass prepare-plus-build acceptance on native amd64 and arm64 before changing any member.

### `prepare` delivery across the host matrix

Railpack release binaries are built with `CGO_ENABLED=0` for Linux amd64 and arm64 and named as musl targets ([GoReleaser build matrix](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/.goreleaser.yml#L8-L38)). That is the upstream artifact shape suitable for ADR-0034's mostly-equal but distro-diverse Linux hosts.

The tempting alternative—run `prepare` by overriding the exact frontend image's entrypoint—is not a sound beta contract. The frontend image is Alpine and contains only the Railpack binary ([frontend Dockerfile](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/images/alpine/frontend/goreleaser.Dockerfile#L1-L7)). During plan generation, Railpack ensures its pinned `mise` exists by downloading the platform's standard `linux-x64` or `linux-arm64` archive and then executing it; the asset selection has no musl variant ([mise installer](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/core/mise/install.go#L25-L101), [download and validation](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/core/mise/install.go#L103-L139), [execution](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/core/mise/mise.go#L20-L42)). A refreshed direct amd64 probe ran `railpack prepare` in the digest-pinned `v0.31.0` frontend image; it downloaded `mise` `2026.7.5` and then failed to execute it with `no such file or directory`, consistent with a non-Alpine dynamic-loader mismatch. Upstream does not advertise the frontend image as a `prepare` container; its documented role is the BuildKit frontend.

Fetching the CLI per build is also unnecessary: the release already supplies checksum-addressable binaries, and per-build fetching would add an external availability step to every bounded operation.

**Ployz consequence:** include the two checksum-verified official Railpack Linux CLI assets in Ployz release material and install the native one into a private versioned libexec path (not global `PATH`). `ployzd` invokes that exact helper for `prepare`; the ordinary Ployz Host Runner/substrate version relationship distributes it to amd64 and arm64 machines. Do not install from distro packages, do not resolve `latest`, and do not fetch it during a build. The matching digest-pinned Railpack frontend remains a BuildKit input image, not the prepare runtime.

### Mount-cache isolation semantics

Railpack's production guide says mount-cache IDs default to the cached directory and recommends `cache-key` for multi-tenant isolation; that key is prefixed to every mount-cache ID ([production guide](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/docs/src/content/docs/guides/running-railpack-in-production.mdx#L112-L123)). The frontend reads the `cache-key` build argument and passes it into plan conversion ([frontend source](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/buildkit/frontend.go#L26-L75)). The cache store implements the rule literally as `<uniqueID>-<plan cache key>` and uses the unprefixed plan key when no unique ID is supplied ([cache-store source](https://github.com/railwayapp/railpack/blob/ebb3c1ccbd6ca2386711d21197d74681d33b4df7/buildkit/build_llb/cache_store.go#L17-L43)). BuildKit cache mounts persist compiler/package-manager state outside final image layers, and sharing modes govern concurrent use rather than tenant authorization ([BuildKit Dockerfile cache-mount reference](https://github.com/moby/buildkit/blob/v0.31.1/frontend/dockerfile/docs/reference.md#run---mounttypecache)).

Therefore the cache key has two distinct jobs:

1. **Isolation:** different project/trust boundaries must never produce the same prefix on one machine-local BuildKit state volume.
2. **Reuse:** successive commits and retries for the same project must produce the same prefix; including commit SHA or operation id would isolate every build and defeat mount-cache reuse.

Architecture need not enter the scope: the cache state is machine-local and #513 already places each platform build on a native machine. Railpack and BuildKit versions also need not enter the caller scope; Ployz can bump the internal encoding prefix (`ployz-railpack-v1`) if a future toolchain change requires hard cache separation. BuildKit's ordinary content-addressed layer cache remains separate from Railpack's writable mount-cache IDs.

This is logical Railpack mount-cache isolation, not an adversarial multi-tenant security boundary. BuildKit explicitly says concurrent clients can reuse local cache/cache mounts without namespacing and recommends separate `buildkitd` instances when stronger isolation is required ([BuildKit security boundary](https://github.com/moby/buildkit/blob/v0.31.1/PROJECT.md#examples-of-issues-not-currently-considered-security)). The beta contract prevents accidental cross-project mount-cache reuse inside one trusted Ployz cluster; it must not claim hostile-tenant isolation.

## Acceptance required when implementing or upgrading

- Assert the shipped CLI reports the pinned Railpack release on native amd64 and arm64.
- Generate a plan with that CLI and build it only with the same-release frontend digest.
- Run the pair against the exact pinned BuildKit digest on both native architectures; no QEMU result substitutes for native acceptance.
- Build two projects whose plans use the same cache directory and verify their derived prefixes differ; rebuild each across a new commit and verify its prefix remains stable.
- Record Railpack release, CLI asset SHA-256, frontend index/selected-manifest digest, BuildKit index/selected-manifest digest, and derived cache-scope fingerprint as non-secret per-platform operation evidence.
- Fail before source execution if any installed helper or pulled image digest differs from the release tuple.

## Rejected beta shapes

- `Railpack {}`: cannot express the caller-owned stable cache trust boundary.
- `Railpack { provider: ... }`: duplicates source-owned `railpack.json` behavior.
- `Railpack { version: ... }` or caller-supplied frontend/BuildKit references: turns a tested executor invariant into per-build policy and permits unvalidated combinations.
- Derive cache scope from commit or operation: safe isolation, no useful mount-cache reuse.
- Derive cache scope only from repository URL/subdirectory: no tenant boundary and avoidable cross-project writable-cache sharing.
- Run prepare in the upstream frontend image: the image is built for BuildKit's frontend protocol and its Alpine/mise execution seam is not a supported prepare runtime.
- Fetch Railpack per operation: adds mutable external delivery and failure surface despite release-time multi-arch assets.
