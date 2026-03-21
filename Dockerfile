FROM rust:1.94-slim@sha256:f7bf1c266d9e48c8d724733fd97ba60464c44b743eb4f46f935577d3242d81d0 AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:7e5b8df2f4d36f5599ef4ab856d7d444922531709becb03f3368c6d797d0a5eb

COPY --from=builder /app/target/release/jawnscanner /

EXPOSE 9101

ENTRYPOINT ["/jawnscanner"]
