# syntax=docker/dockerfile:1
#
# The Vanillite server image: one static (musl) binary on an empty base.
#
#   docker build -t vanillite .                       builds the binary in the image
#   cargo xtask docker [--push]                       builds the binaries on the host (faster,
#                                                     sccache) and uses BINARY=prebuilt
#
# The build stage runs on the build machine and cross-compiles for each target platform with
# cargo-zigbuild, so multi-arch builds (linux/amd64, linux/arm64) need no emulation.

ARG BINARY=build

FROM --platform=$BUILDPLATFORM rust:1.99.0-slim-bookworm AS build
ARG ZIG_VERSION=0.14.1
ARG ZIGBUILD_VERSION=0.24.1
RUN apt-get update && apt-get install -y --no-install-recommends curl xz-utils ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/zig-$(uname -m)-linux-${ZIG_VERSION}.tar.xz" \
       | tar -xJ -C /opt && ln -s /opt/zig-*/zig /usr/local/bin/zig \
    && cargo install --locked cargo-zigbuild --version "${ZIGBUILD_VERSION}" \
    && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
WORKDIR /src
COPY . .
ARG TARGETARCH
# The commit shown by `vanillite --version` (the build context has no .git).
ARG VANILLITE_COMMIT=
ENV VANILLITE_COMMIT=${VANILLITE_COMMIT}
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,id=vanillite-target-${TARGETARCH} \
    case "${TARGETARCH}" in \
      amd64) triple=x86_64-unknown-linux-musl ;; \
      arm64) triple=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported architecture ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    # zig's clang turns off evex512 for the x86-64 baseline; libdeflate's AVX-512 code needs it.
    && CFLAGS_x86_64_unknown_linux_musl=-mevex512 cargo zigbuild --release --locked -p vanillite --target "${triple}" \
    && mkdir -p /out && cp "target/${triple}/release/vanillite" /out/vanillite \
    # amd64: also the x86-64-v3 build (AVX2), which the baseline binary starts where the CPU has it.
    && if [ "${TARGETARCH}" = amd64 ]; then \
         RUSTFLAGS="-C target-cpu=x86-64-v3" CFLAGS_x86_64_unknown_linux_musl=-mevex512 \
           cargo zigbuild --release --locked -p vanillite --target "${triple}" --target-dir target/v3 \
         && cp "target/v3/${triple}/release/vanillite" /out/vanillite-x86-64-v3; \
       fi

FROM scratch AS binary-build
COPY --from=build /out/ /

FROM scratch AS binary-prebuilt
ARG TARGETARCH
COPY target/docker/${TARGETARCH}/ /

FROM binary-${BINARY} AS binary

# The data folder, owned by the server user (an empty base has no mkdir), and the applets of a
# static busybox for the entrypoint (Pterodactyl's STARTUP command). The busybox binary comes
# from the target platform's image by COPY, so multi-arch builds need no emulation.
FROM --platform=$BUILDPLATFORM busybox:stable-musl AS data
RUN mkdir -p /rootfs/data /rootfs/tmp /rootfs/bin && chown 10001:10001 /rootfs/data \
    && chmod 1777 /rootfs/tmp \
    && for applet in sh sed env; do ln -s busybox "/rootfs/bin/${applet}"; done

FROM busybox:stable-musl AS busybox

FROM scratch
COPY --from=data /rootfs/ /
COPY --from=busybox /bin/busybox /bin/busybox
COPY --from=binary / /
COPY --chmod=755 scripts/docker/entrypoint.sh /entrypoint.sh
USER 10001:10001
WORKDIR /data
VOLUME /data
EXPOSE 25565/tcp
# Every key of vanillite.toml and server.properties can be set as VANILLITE_<KEY>, e.g.
# VANILLITE_SERVER_PORT, VANILLITE_MOTD, VANILLITE_PERFORMANCE__TICK__PARALLEL_DIMENSIONS.
# VANILLITE_EULA=true accepts the Minecraft EULA. An empty variable is ignored.
# Pterodactyl: the egg deploy/pterodactyl/egg-vanillite.json (STARTUP, /home/container).
ENV VANILLITE_LOG=info
# mimalloc (the musl build's allocator): commit arena memory on use and return freed pages at
# once. Measured (#164): ~15 MiB less idle RSS, 50-100 MiB less under load and while chunks
# generate, and a faster start, at the same tick time as the default.
ENV MIMALLOC_ARENA_EAGER_COMMIT=0 MIMALLOC_PURGE_DELAY=0
# The first start downloads the Vanilla jar and generates the spawn area.
HEALTHCHECK --interval=30s --timeout=10s --start-period=5m --retries=3 \
    CMD ["/vanillite", "health", "--dir", "/data"]
# SIGTERM saves all worlds and players, then exits; give it time (docker stop -t 60).
STOPSIGNAL SIGTERM
ENTRYPOINT ["/entrypoint.sh"]
CMD ["run", "--dir", "/data"]
