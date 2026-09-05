FROM rust:1.85-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY apps ./apps
COPY packages ./packages
RUN cargo build --locked --release -p execution-gateway

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /build/target/release/execution-gateway /usr/local/bin/execution-gateway
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/execution-gateway"]
