# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.94.0
ARG API_RUNTIME=api-runtime

FROM rust:${RUST_VERSION}-bookworm AS planner
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN sed -i '/^members = \[/,/^\]/c\members = ["apps/api", "crates/*"]' Cargo.toml
COPY crates crates
COPY apps/api/Cargo.toml apps/api/Cargo.toml
RUN mkdir -p apps/api/src && echo "fn main() {}" > apps/api/src/main.rs
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:${RUST_VERSION}-bookworm AS build
ARG APP_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev libclang-dev ca-certificates && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked
ENV APP_VERSION=${APP_VERSION}
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
# cargo-chef does not include this non-workspace path dependency in its recipe.
COPY crates/tiptap/vendor crates/tiptap/vendor
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo chef cook --release --recipe-path recipe.json -p api
COPY Cargo.toml Cargo.lock ./
RUN sed -i '/^members = \[/,/^\]/c\members = ["apps/api", "crates/*"]' Cargo.toml
COPY crates crates
COPY packages/pricing/src/trial-policy.json packages/pricing/src/trial-policy.json
COPY apps/api apps/api
ARG GH_SHA
ENV GH_SHA=${GH_SHA}
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo clean -p progenitor-utils
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo build --locked --release -p api

FROM debian:bookworm-slim AS api-runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
RUN groupadd -g 1001 appgroup && \
    useradd -u 1001 -g appgroup -d /nonexistent -s /usr/sbin/nologin -M appuser
COPY --from=build --chown=appuser:appgroup /app/target/release/api /usr/local/bin/api
USER appuser
WORKDIR /app
EXPOSE 3001
ENTRYPOINT ["/usr/local/bin/api"]

FROM node:22-slim AS billing-node-base
WORKDIR /repo

ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
ENV CI=true
RUN corepack enable

FROM billing-node-base AS billing-deps
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches patches
COPY apps/stripe/package.json apps/stripe/package.json
COPY packages/supabase/package.json packages/supabase/package.json
COPY packages/user-error/package.json packages/user-error/package.json
RUN pnpm fetch --prod --filter @anlg/stripe

FROM billing-deps AS billing-build
COPY apps/stripe apps/stripe
COPY packages/supabase packages/supabase
COPY packages/user-error packages/user-error
RUN pnpm install --filter @anlg/stripe --frozen-lockfile
# The filtered Stripe runtime excludes mobile-only patched dependencies.
RUN pnpm --config.allowUnusedPatches=true deploy --filter @anlg/stripe --prod --legacy /runtime

FROM oven/bun:1 AS bun-runtime

FROM api-runtime AS billing-runtime
ARG APP_VERSION
ENV APP_VERSION=${APP_VERSION}
COPY --from=bun-runtime /usr/local/bin/bun /usr/local/bin/bun
COPY --from=billing-build --chown=appuser:appgroup /runtime /app/billing
WORKDIR /app/billing
ENTRYPOINT ["bun", "src/index.ts"]

FROM build AS whisper-build
RUN apt-get update && apt-get install -y --no-install-recommends cmake g++ curl && rm -rf /var/lib/apt/lists/*
WORKDIR /whisper
RUN curl --fail --location --retry 3 --max-time 180 https://codeload.github.com/ggml-org/whisper.cpp/tar.gz/refs/tags/v1.8.1 -o source.tar.gz \
    && echo 'b7c6b05635e5fda85cbcc5a012d29b3a4ca1cc22d9fa54d5b6c33e4ac271e5e0  source.tar.gz' | sha256sum --check \
    && tar -xzf source.tar.gz --strip-components=1 \
    && cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=OFF -DGGML_CUDA=OFF -DGGML_METAL=OFF -DWHISPER_BUILD_TESTS=OFF \
    && cmake --build build --target whisper-server --parallel 2

FROM api-runtime AS recordings-runtime
USER root
RUN apt-get update && apt-get install -y --no-install-recommends python3 libgomp1 libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 recorder \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin recorder
COPY --from=whisper-build /whisper/build/bin/whisper-server /usr/local/bin/whisper-server
COPY apps/api/recordings/entrypoint.py /opt/recordings/entrypoint.py
COPY apps/api/recordings/third-party /usr/share/doc/anarlog-recordings/third-party
ENV PORT=8080 PYTHONDONTWRITEBYTECODE=1
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=3s --start-period=240s --retries=3 CMD ["python3", "/opt/recordings/entrypoint.py", "healthcheck"]
ENTRYPOINT ["python3", "/opt/recordings/entrypoint.py"]

FROM ${API_RUNTIME} AS runtime
