FROM rust:1.97 AS builder
# aws-lc-rs, the crypto library behind reqwest's rustls backend, builds with
# cmake, which the rust image does not ship.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /putio/
COPY ./ ./
RUN cargo build --release --locked

# Unfortunately some of the CLI dependencies do not allow building a
# statically linked binary. Using Ubuntu as the runner instead of a
# lighter image like Alpine because of known incompatiblities between
# the minimal libc (musl) that ships with Alpine and the more typical
# libc the binary is built using.
FROM ubuntu:24.04 AS runner
COPY --from=builder /putio/target/release/putio /usr/bin/putio
RUN apt-get update \
    && apt-get install -y --no-install-recommends aria2 wget curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
