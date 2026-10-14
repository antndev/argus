FROM rust:1-alpine AS chef
RUN apk add --no-cache musl-dev && cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src

FROM chef AS plan
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY --from=plan /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p argus-agent --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked -p argus-agent && cp target/release/argus-agent /argus-agent

FROM scratch
COPY --from=build /argus-agent /argus-agent
ENTRYPOINT ["/argus-agent"]
