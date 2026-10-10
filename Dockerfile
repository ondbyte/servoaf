# Build image for servoaf: `make docker-build` builds target/release/servoaf
# with the host checkout mounted at /src.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
COPY scripts/install-deps.sh rust-toolchain.toml /tmp/servoaf/scripts/
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl sudo && \
    mv /tmp/servoaf/scripts/rust-toolchain.toml /tmp/servoaf/ && \
    /tmp/servoaf/scripts/install-deps.sh && \
    rm -rf /var/lib/apt/lists/*
ENV PATH=/root/.cargo/bin:$PATH
WORKDIR /src
