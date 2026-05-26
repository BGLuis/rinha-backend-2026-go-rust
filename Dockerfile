# --- Estágio Base Rust: Cache de Dependências ---
FROM rust:1.82-slim-bookworm AS rust-env
ARG TARGETARCH
WORKDIR /build/rust_engine

# Copia apenas o manifesto para cachear o download e compilação de crates
COPY rust_engine/Cargo.toml ./
RUN mkdir src && echo "pub fn dummy() {}" > src/lib.rs && \
    mkdir -p src/bin && echo "fn main() {}" > src/bin/build_index.rs

# Flags agressivas de hardware (SIMD/AVX2) apenas para x86_64
RUN if [ "$TARGETARCH" = "amd64" ]; then \
    export RUSTFLAGS="-C target-cpu=haswell -C target-feature=+avx2,+fma,+f16c,+bmi2,+popcnt -C link-arg=-s"; \
    else \
    export RUSTFLAGS="-C link-arg=-s"; \
    fi && \
    cargo build --release && rm -rf src

# --- Estágio Builder Rust: Compilação da Lib ---
FROM rust-env AS rust-builder
ARG TARGETARCH
COPY rust_engine/src ./src
# Força o rebuild apenas do código da aplicação
RUN if [ "$TARGETARCH" = "amd64" ]; then \
    export RUSTFLAGS="-C target-cpu=haswell -C target-feature=+avx2,+fma,+f16c,+bmi2,+popcnt -C link-arg=-s"; \
    else \
    export RUSTFLAGS="-C link-arg=-s"; \
    fi && \
    touch src/lib.rs && cargo build --release

# --- Estágio Dataset: Geração do Index (v2) ---
FROM rust-env AS dataset-builder
ARG TARGETARCH
# Mantemos o WORKDIR em /build/rust_engine onde o Cargo.toml já existe
COPY resources/ ./resources/
COPY rust_engine/src ./src
# Gera o dataset.bin.
RUN if [ "$TARGETARCH" = "amd64" ]; then \
    export RUSTFLAGS="-C target-cpu=haswell -C target-feature=+avx2,+fma,+f16c,+bmi2,+popcnt -C link-arg=-s"; \
    else \
    export RUSTFLAGS="-C link-arg=-s"; \
    fi && \
    touch src/bin/build_index.rs && cargo run --release --bin build_index && ls -lh dataset.bin

# --- Estágio Builder Go: API com CGO ---
FROM golang:1.24-bookworm AS go-builder
WORKDIR /app

# Copia arquivos do Go API
COPY go_api/go.mod ./
COPY go_api/main.go .

# Resolve e baixa dependências baseadas no código
RUN go mod tidy && go mod download

# Copia lib estática do Rust
COPY --from=rust-builder /build/rust_engine/target/release/librust_engine.a /usr/local/lib/

# Compilação Go vinculando a lib Rust via CGO
RUN CGO_ENABLED=1 GOOS=linux go build -ldflags="-s -w" -o rinha-api main.go

# --- Estágio Final: Imagem de Produção Enxuta ---
FROM debian:bookworm-slim
WORKDIR /app

# Artefatos finais
COPY --from=go-builder /app/rinha-api .
# O dataset.bin foi gerado no WORKDIR do dataset-builder (/build/rust_engine)
COPY --from=dataset-builder /build/rust_engine/dataset.bin .
COPY resources/normalization.json ./resources/
COPY resources/mcc_risk.json ./resources/

# Configurações de runtime
ENV DATASET_PATH=/app/dataset.bin
ENV GOMAXPROCS=1

CMD ["./rinha-api"]
