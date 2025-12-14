# Use the official Rust image as the base
FROM rust:latest AS builder

# Set working directory
WORKDIR /app

# Copy Cargo files
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src

# Copy only the cache file
COPY hikari-dance/frames_cache.bin ./hikari-dance/frames_cache.bin

# Build the application in release mode
RUN cargo build --release

# Use the same base image as the builder to avoid GLIBC issues
FROM rust:latest

# Install OpenSSH client (already included in rust image)
RUN apt-get update && apt-get install -y \
    openssh-client \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user for security
RUN useradd -m -s /bin/bash appuser

# Create app directory and authorized_keys directory with SSH key pair
RUN mkdir -p /app/authorized_keys && \
    cd /app/authorized_keys && \
    ssh-keygen -t ed25519 -f id_ed25519 -N "" -C "container-generated-key" && \
    chmod 600 id_ed25519 && \
    chmod 644 id_ed25519.pub && \
    chown -R appuser:appuser /app/authorized_keys

# Set working directory
WORKDIR /app

# Copy the compiled binary from the builder stage
COPY --from=builder /app/target/release/portfolio-v2 /app/portfolio-v2

# Copy only the cache file to the final image
COPY --from=builder /app/hikari-dance/frames_cache.bin /app/hikari-dance/frames_cache.bin

# Ensure the cache file is properly treated as a file
RUN ls -la /app/hikari-dance/frames_cache.bin && \
    file /app/hikari-dance/frames_cache.bin

# Change ownership to the appuser
RUN chown -R appuser:appuser /app

# Switch to the non-root user
USER appuser

# Expose SSH port
EXPOSE 22

# Set the entrypoint to run the SSH server
ENTRYPOINT ["./portfolio-v2", "--server"]
