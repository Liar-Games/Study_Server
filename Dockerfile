FROM rust:latest AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release

COPY src ./src

RUN cargo build --release
ENV BINARY_NAME=study_server



FROM debian:bookworm-slim
COPY --from=builder /app/target/release/${BINARY_NAME} /usr/local/bin/

ENV PORT=8080
ENV RUST_LOG=info

CMD ["/usr/local/bin/${BINARY_NAME}"]

EXPOSE 8080