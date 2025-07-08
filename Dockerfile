FROM rust:1.86 AS builder
WORKDIR /putio/
COPY ./ ./
RUN cargo build --release

# Unfortunately some of the CLI dependencies do not allow building a
# statically linked binary. Using Ubuntu as the runner instead of a
# lighter image like Alpine because of known incompatiblities between
# the minimal libc (musl) that ships with Alpine and the more typical
# libc the binary is built using.
FROM ubuntu:24.04 AS runner
COPY --from=builder /putio/target/release/putio /usr/bin/putio
RUN apt update && apt install -y aria2 wget curl
