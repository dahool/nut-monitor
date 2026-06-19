# --- Etapa de Compilación ---
FROM rust:1.96-slim-bookworm AS builder
WORKDIR /app

# Instalar dependencias esenciales de compilación
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Truco para cachear dependencias de Cargo
COPY Cargo.toml ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -f src/main.rs target/release/deps/nut_monitor_web*

# Copiar el código fuente real y compilar
COPY src ./src
RUN cargo build --release

# --- Etapa de Ejecución ---
FROM debian:bookworm-slim
WORKDIR /app

# Instalar obligatoriamente nut-client para disponer del binario `upsc`
RUN apt-get update && apt-get install -y --no-install-recommends \
    nut-client \
    && rm -rf /var/lib/apt/lists/*

# Copiar el binario compilado desde la etapa anterior
COPY --from=builder /app/target/release/nut-monitor-web .

# Exponer el puerto de la app en Rust
EXPOSE 3000

CMD ["./nut-monitor-web"]