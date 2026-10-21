ARG RUST_IMAGE=rust:1.94.0-bookworm@sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

FROM ${RUST_IMAGE} AS builder
RUN apt-get update && apt-get install --no-install-recommends -y cmake g++ pkg-config libssl-dev libclang-dev ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /source
COPY . .
RUN cargo build --locked --manifest-path enterprise/Cargo.toml --package anarlog-enterprise-control-plane --bin shared-recordings-app --release

WORKDIR /whisper
RUN curl --fail --location --retry 3 --max-time 180 https://codeload.github.com/ggml-org/whisper.cpp/tar.gz/refs/tags/v1.8.1 -o source.tar.gz \
    && echo 'b7c6b05635e5fda85cbcc5a012d29b3a4ca1cc22d9fa54d5b6c33e4ac271e5e0  source.tar.gz' | sha256sum --check \
    && tar -xzf source.tar.gz --strip-components=1 \
    && cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=OFF -DGGML_CUDA=OFF -DGGML_METAL=OFF -DWHISPER_BUILD_TESTS=OFF \
    && cmake --build build --target whisper-server --parallel 2

FROM caddy:2.10.2@sha256:c3d7ee5d2b11f9dc54f947f68a734c84e9c9666c92c88a7f30b9cba5da182adb AS proxy
FROM ${RUNTIME_IMAGE}
RUN apt-get update && apt-get install --no-install-recommends -y ca-certificates libssl3 libstdc++6 libgomp1 python3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 anarlog \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin anarlog
COPY --from=builder /source/enterprise/target/release/shared-recordings-app /usr/local/bin/shared-recordings-app
COPY --from=builder /whisper/build/bin/whisper-server /usr/local/bin/whisper-server
COPY --from=builder /whisper/LICENSE /usr/share/doc/whisper.cpp/LICENSE
COPY --from=proxy /usr/bin/caddy /usr/local/bin/caddy
COPY enterprise/control-plane/preview-hosting/entrypoint.py /opt/preview/entrypoint.py
COPY enterprise/control-plane/preview-hosting/Caddyfile /opt/preview/Caddyfile
COPY enterprise/control-plane/preview-hosting/third-party /usr/share/doc/anarlog-shared-preview/third-party
ENV PORT=8080 PYTHONDONTWRITEBYTECODE=1
EXPOSE 8080
HEALTHCHECK --interval=15s --timeout=3s --start-period=240s --retries=3 CMD ["python3", "/opt/preview/entrypoint.py", "healthcheck"]
ENTRYPOINT ["python3", "/opt/preview/entrypoint.py"]
