FROM rust:1.93.1-bookworm AS build
WORKDIR /build/saic-rust-notify
COPY saic-rust-client/Cargo.toml /build/saic-rust-client/Cargo.toml
COPY saic-rust-client/src /build/saic-rust-client/src
COPY saic-rust-notify/Cargo.toml saic-rust-notify/Cargo.lock ./
COPY saic-rust-notify/src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=build /build/saic-rust-notify/target/release/saic-rust-notify /usr/local/bin/saic-rust-notify
ENV HTTP_HOST=0.0.0.0 HTTP_PORT=8080
RUN mkdir /app/data && chown 10001:10001 /app/data
VOLUME ["/app/data"]
USER 10001:10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/saic-rust-notify"]
