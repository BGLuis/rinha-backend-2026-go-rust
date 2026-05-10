# --- Estágio Base Rust: Cache de Dependências ---
FROM rust:1.82-slim-bookworm AS rust-env
WORKDIR /build/rust_engine

# Copia apenas o manifesto para cachear o download e compilação de crates
COPY rust_engine/Cargo.toml ./
RUN mkdir src && echo "pub fn dummy() {}" > src/lib.rs && \
    mkdir -p src/bin && echo "fn main() {}" > src/bin/build_index.rs

# Flags agressivas de hardware (SIMD/AVX2)
ENV RUSTFLAGS="-C target-cpu=haswell -C target-feature=+avx2,+fma,+f16c,+bmi2,+popcnt -C link-arg=-s"

# Baixa e compila dependências (camada pesada e estável)
RUN cargo build --release && rm -rf src

# --- Estágio Builder Rust: Compilação da Lib ---
FROM rust-env AS rust-builder
COPY rust_engine/src ./src
# Força o rebuild apenas do código da aplicação
RUN touch src/lib.rs && cargo build --release

# --- Estágio Dataset: Geração do Index ---
FROM rust-env AS dataset-builder
# Mantemos o WORKDIR em /build/rust_engine onde o Cargo.toml já existe
COPY resources/ /build/resources/
COPY rust_engine/src ./src
# Gera o dataset.bin. Ajustamos o caminho de saída se necessário no comando
RUN touch src/bin/build_index.rs && cargo run --release --bin build_index

# --- Estágio Builder Go: API com CGO ---
FROM golang:1.22-bookworm AS go-builder
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

# Instala certificados necessários para HTTPS
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Artefatos finais
COPY --from=go-builder /app/rinha-api .
# O dataset.bin foi gerado no WORKDIR do dataset-builder (/build/rust_engine)
COPY --from=dataset-builder /build/rust_engine/dataset.bin .
COPY resources/normalization.json ./resources/
COPY resources/mcc_risk.json ./resources/

# Configurações de runtime
ENV DATASET_PATH=/app/dataset.bin
ENV GOMAXPROCS=2

CMD ["./rinha-api"]
