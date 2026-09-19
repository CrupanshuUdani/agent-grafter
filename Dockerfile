# Builds `agent-grafter` as a fully static Linux binary (x86_64-unknown-linux-musl,
# no libc dependency) and exposes it as a `scratch`-based export stage that CI
# extracts via `docker buildx build --output`, without ever needing a runnable
# image.
#
# CI usage (Task 5, S5 CI pipeline hardening):
#
#   docker buildx build --target export --output type=local,dest=dist .
#
# This writes the binary to `dist/agent-grafter` in the build context on the
# runner's filesystem -- no `docker create` / `docker cp` container dance
# needed. Task 6 (multi-distro smoke test) consumes `dist/agent-grafter`.

# rust:*-alpine ships a musl-based toolchain, so `x86_64-unknown-linux-musl`
# is effectively the native target here -- no cross-toolchain juggling.
FROM rust:1-alpine AS builder

# musl-dev + a linker are required to link anything (including the musl libc
# itself) statically; without musl-dev, the stdlib symbols won't resolve.
RUN apk add --no-cache musl-dev

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# RUSTFLAGS: target-feature=+crt-static is the actual switch that produces a
# genuinely static binary on musl -- the musl target alone does not guarantee
# it (musl libc itself can still end up dynamically linked). This produces a
# static-PIE binary (`file` reports "static-pie linked", not the older plain
# "statically linked" wording) -- that is still genuinely static: zero
# PT_INTERP, zero DT_NEEDED entries. See the task report for the ldd/file/
# readelf proof this was checked, not assumed.
RUN RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target x86_64-unknown-linux-musl -p agent-grafter

# Export-only stage: contains nothing but the binary. Never meant to be run
# as a container -- only to be extracted with `docker buildx build --output`.
FROM scratch AS export
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/agent-grafter /agent-grafter
