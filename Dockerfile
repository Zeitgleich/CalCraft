# syntax=docker/dockerfile:1

FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p calcraft-server \
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
