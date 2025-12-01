FROM rust:slim AS builder
WORKDIR /app

COPY ./Cargo.toml ./Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release && rm -rf src

COPY . .
RUN cargo build --release



FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/study_server/target/release/study_server /usr/local/bin/server
ENV PORT=8080
ENV RUST_LOG=info
EXPOSE 8080
CMD ["server"]