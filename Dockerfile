# syntax=docker/dockerfile:1

FROM rust:1.99-bookworm AS build
WORKDIR /src
# Must match the wasm-bindgen version pinned in crates/calcraft-ui/Cargo.toml.
ARG WASM_BINDGEN_VERSION=0.2.114
RUN rustup target add wasm32-unknown-unknown \
    && cargo install wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION" --locked
COPY . .
# Web UI assets first; the server build embeds them and fails without them.
RUN scripts/build-ui.sh \
    && CALCRAFT_UI_REQUIRED=1 cargo build --release --locked -p calcraft-server \
    && mkdir -p /data

# Distroless: no shell, no package manager, runs as non-root.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/calcraft /usr/local/bin/calcraft
COPY --from=build --chown=65532:65532 /data /data
ENV CALCRAFT_SERVER__BIND=0.0.0.0:5232 \
    CALCRAFT_STORAGE__DATABASE=/data/calcraft.db
VOLUME ["/data"]
EXPOSE 5232
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/calcraft"]
CMD ["serve"]
