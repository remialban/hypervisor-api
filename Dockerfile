FROM rust:1-bookworm AS builder

WORKDIR /build

RUN apt-get update \
    && apt-get install --no-install-recommends -y libvirt-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates libvirt0 openssh-client \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/api /app/api

RUN mkdir -p /root/.ssh \
    && printf '%s\n' \
        'IdentityFile /run/secrets/ssh_private_key' \
        'IdentitiesOnly yes' \
        'UserKnownHostsFile /dev/null' \
        'StrictHostKeyChecking no' \
        > /root/.ssh/config \
    && chmod 600 /root/.ssh/config

EXPOSE 3000

CMD ["/app/api"]
