# Builds `agent-grafter` as a fully static Linux binary (musl, for x86_64 or
# aarch64, no libc dependency) and exposes it as a `scratch`-based export
# stage that CI extracts via `docker buildx build --output`, without ever
# needing a runnable image.
#
# CI usage (Task 5, S5 CI pipeline hardening):
#
#   docker buildx build --target export --output type=local,dest=dist .
#
# This writes the binary to `dist/agent-grafter` in the build context on the
# runner's filesystem -- no `docker create` / `docker cp` container dance
# needed. Task 6 (multi-distro smoke test) consumes `dist/agent-grafter`.

FROM rust:1-alpine AS builder

# musl-dev + a linker are required to link anything (including the musl libc
# itself) statically; without musl-dev, the stdlib symbols won't resolve.
RUN apk add --no-cache musl-dev

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# RUSTFLAGS: target-feature=+crt-static is the actual switch that produces a
# genuinely static binary on musl -- the musl target alone does not guarantee
# it (musl libc itself can still end up dynamically linked). On x86_64 `file`
# reports "static-pie linked"; on aarch64 it reports "statically linked".
# Both are still genuinely static: zero PT_INTERP, zero DT_NEEDED entries.
# The host triple of rust:*-alpine is the musl target for whichever
# architecture buildx selected, so one Dockerfile serves amd64 and arm64.
RUN target="$(rustc -vV | sed -n 's/^host: //p')" \
 && RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --locked --target "$target" -p agent-grafter \
 && cp "target/$target/release/agent-grafter" /agent-grafter

# Export-only stage: contains nothing but the binary. Never meant to be run
# as a container -- only to be extracted with `docker buildx build --output`.
FROM scratch AS export
COPY --from=builder /agent-grafter /agent-grafter
