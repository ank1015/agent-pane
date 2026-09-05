FROM rust:1.85-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY apps ./apps
COPY packages ./packages
RUN cargo build --locked --release -p llm-gateway

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /build/target/release/llm-gateway /usr/local/bin/llm-gateway
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/llm-gateway"]
