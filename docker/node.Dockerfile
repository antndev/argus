FROM rust:1-alpine AS chef
RUN apk add --no-cache musl-dev && cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src

FROM chef AS plan
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:1-bookworm AS ui
ARG TRUNK_VERSION=0.21.14
RUN rustup target add wasm32-unknown-unknown \
 && cargo install cargo-chef --locked --version 0.1.78 \
 && release="https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-$(uname -m)-unknown-linux-gnu.tar.gz" \
 && curl -fsSLo /tmp/trunk.tar.gz "$release" \
 && echo "$(curl -fsSL "$release.sha256")  /tmp/trunk.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/trunk.tar.gz -C /usr/local/bin \
 && rm /tmp/trunk.tar.gz
WORKDIR /src
COPY --from=plan /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --target wasm32-unknown-unknown -p argus-ui --recipe-path recipe.json
COPY . .
RUN NO_COLOR=true trunk build --release --locked

FROM chef AS node
COPY --from=plan /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p argus-node --recipe-path recipe.json \
 && cargo chef cook --release --locked -p argus-agent --recipe-path recipe.json
COPY . .
COPY --from=ui /src/target/ui target/ui
RUN cargo build --release --locked -p argus-node -p argus-agent && mkdir -p /out/data && cp target/release/argus-node target/release/argus-agent /out/

FROM scratch
COPY --from=node /out/argus-node /out/argus-agent /
COPY --from=node --chown=65534:65534 /out/data /data
USER 65534:65534
VOLUME /data
EXPOSE 8090
ENTRYPOINT ["/argus-node"]
CMD ["serve", "--config", "/data/argus.json"]
