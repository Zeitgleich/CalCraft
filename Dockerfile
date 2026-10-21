# syntax=docker/dockerfile:1

FROM rust:1.93-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p calcraft-server

# Distroless: no shell, no package manager, runs as non-root.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/calcraft /usr/local/bin/calcraft
ENV CALCRAFT_SERVER__BIND=0.0.0.0:5232
EXPOSE 5232
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/calcraft"]
CMD ["serve"]
