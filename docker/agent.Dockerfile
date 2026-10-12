FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p argus-agent && cp target/release/argus-agent /argus-agent

FROM scratch
COPY --from=build /argus-agent /argus-agent
ENTRYPOINT ["/argus-agent"]
