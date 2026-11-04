ARG RUST_IMAGE=rust:1.91.1-bookworm
FROM ${RUST_IMAGE} AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
COPY examples ./examples
RUN cargo build --locked --release --all-targets
FROM build AS test
CMD ["cargo", "test", "--locked", "--all-targets"]
FROM debian:bookworm-slim AS runtime
RUN useradd --system --uid 10001 --no-create-home engine
COPY --from=build /app/target/release/study_server /usr/local/bin/study-server
USER 10001:10001
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["study-server"]
