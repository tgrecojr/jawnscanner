FROM rust:1.82-slim AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /app/target/release/jawnscanner /

EXPOSE 9101

ENTRYPOINT ["/jawnscanner"]
