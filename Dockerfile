# Étape 1 : Compilation
FROM rust:trixie AS builder

# Dépendances système de compilation (libvirt C headers, pkg-config et curl pour utoipa-swagger-ui)
RUN apt-get update && apt-get install -y \
    pkg-config \
    libvirt-dev \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/api

COPY . .

# Compilation en mode release
RUN cargo build --release

# Étape 2 : Image finale d'exécution légère
FROM debian:trixie-slim

# Dépendances système d'exécution (bibliothèque libvirt et client ssh)
RUN apt-get update && apt-get install -y \
    libvirt0 \
    openssh-client \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copie du binaire compilé depuis l'étape de build
COPY --from=builder /usr/src/api/target/release/api /usr/local/bin/api

EXPOSE 3000

CMD ["api"]
