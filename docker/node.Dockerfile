FROM rust:1-bookworm AS ui
ARG TRUNK_VERSION=0.21.14
RUN rustup target add wasm32-unknown-unknown \
 && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-x86_64-unknown-linux-gnu.tar.gz" | tar -xz -C /usr/local/bin
WORKDIR /src
COPY . .
RUN NO_COLOR=true trunk build --release

FROM rust:1-alpine AS node
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
COPY --from=ui /src/target/ui target/ui
RUN cargo build --release -p argus-node && mkdir -p /out/data && cp target/release/argus-node /out/argus-node

FROM scratch
COPY --from=node /out/argus-node /argus-node
COPY --from=node --chown=65534:65534 /out/data /data
USER 65534:65534
VOLUME /data
EXPOSE 8090
ENTRYPOINT ["/argus-node"]
CMD ["serve", "--config", "/data/argus.json"]
