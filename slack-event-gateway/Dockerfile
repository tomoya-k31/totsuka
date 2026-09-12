# The Event Gateway image (#660, ADR-0072 decision 9).
#
# Two properties drive every choice here. The container runs in the operator's
# own cloud project, and it sees every Slack message that operator can see — so
# the attack surface it carries is surface they inherit. And it is distroless,
# so **there is nothing in the image to patch a CVE in except this build**: no
# package manager, no shell, no libc to upgrade separately.
#
# Hence: statically linked against musl, `FROM scratch`-adjacent distroless,
# non-root, no shell. What is left to keep current is the Rust toolchain and
# `Cargo.lock`, which is the same list `cargo audit` already watches.
#
# linux/amd64 only. Cloud Run runs amd64; a multi-arch build would double the
# release time to produce an image nothing pulls.

# ---- build ----------------------------------------------------------------
# Alpine's Rust targets musl natively, so there is no cross-compilation setup
# to get wrong. `ring` (the TLS backend this lockfile resolves to) needs a C
# compiler and nothing else.
# Pinned by digest, for the same reason every GitHub Action in this repository
# is pinned by SHA: a tag is mutable, and "the image we build from" is the
# start of the supply chain for a container that holds Slack signing secrets.
# The tag is in the comment so the next person knows what to bump.
FROM rust@sha256:1716b3aa042d735f4566d14dc54e8037de9d69556e2d5dd58131d93a613d173d AS build
# rust:1.98-alpine3.24

RUN apk add --no-cache musl-dev

WORKDIR /src

# Dependencies first, as their own layer: they change on a lockfile bump, the
# source changes on every commit, and building them again for a one-line edit
# is the difference between a 30-second and a 4-minute release job.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src ./src
# The dependency layer left a stale fingerprint for our own crate; without
# this, cargo reuses the placeholder binary and the image ships a program that
# does nothing. It fails silently, which is why it is worth a line.
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --locked

# ---- runtime --------------------------------------------------------------
# `static` rather than `base`: the binary is statically linked, so there is no
# libc to provide, and every file the image does not contain is one that cannot
# have a CVE. `nonroot` runs as uid 65532.
#
# TLS roots are compiled into the binary (see Cargo.toml), so this image is not
# required to ship `ca-certificates` — swapping it for `scratch` would still
# work, which is deliberate: a base-image change must not be able to break TLS
# at runtime only.
FROM gcr.io/distroless/static-debian12@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
# gcr.io/distroless/static-debian12:nonroot

COPY --from=build /src/target/release/slack-event-gateway /slack-event-gateway

# Cloud Run overrides this with its own PORT; the default matches what the
# process falls back to, so `docker run -p 8080:8080` works unconfigured.
ENV PORT=8080
EXPOSE 8080

# No shell in the image, so this is exec form by necessity as well as by
# preference — the process is PID 1 and receives SIGTERM directly, which is
# what lets Cloud Run drain it.
ENTRYPOINT ["/slack-event-gateway"]
