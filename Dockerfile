FROM rust:1.93.1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=build /build/target/release/saic-rust-notify /usr/local/bin/saic-rust-notify
ENV HTTP_HOST=0.0.0.0 HTTP_PORT=8080
USER 10001:10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/saic-rust-notify"]
