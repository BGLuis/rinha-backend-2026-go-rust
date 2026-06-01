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

# --- Estágio Builder Rust: Geração do Index e Compilação da Lib ---
FROM rust-env AS rust-builder
ARG TARGETARCH
# Copia resources para a geração do binário
COPY resources/ ./resources/
COPY rust_engine/src ./src

# 1. Gera o dataset.bin primeiro
RUN if [ "$TARGETARCH" = "amd64" ]; then \
    export RUSTFLAGS="-C target-cpu=haswell -C target-feature=+avx2,+fma,+f16c,+bmi2,+popcnt -C link-arg=-s"; \
    else \
    export RUSTFLAGS="-C link-arg=-s"; \
    fi && \
    touch src/bin/build_index.rs && cargo run --release --bin build_index && ls -lh dataset.bin

# 2. Compila a Lib
RUN if [ "$TARGETARCH" = "amd64" ]; then \
    export RUSTFLAGS="-C target-cpu=haswell -C target-feature=+avx2,+fma,+f16c,+bmi2,+popcnt -C link-arg=-s"; \
    else \
    export RUSTFLAGS="-C link-arg=-s"; \
    fi && \
    touch src/lib.rs && cargo build --release

# --- Estágio PGO: Coleta de perfil para Profile-Guided Optimization ---
FROM golang:1.24-bookworm AS pgo-collector
WORKDIR /app

# Copia código Go e dependências
COPY go_api/go.mod ./
COPY go_api/cmd ./cmd

# Resolve dependências do profiler
RUN go mod tidy && go mod download

# Copia lib estática do Rust
COPY --from=rust-builder /build/rust_engine/target/release/librust_engine.a /usr/local/lib/
COPY --from=rust-builder /build/rust_engine/dataset.bin ./

# Compila e roda o profiler para coletar cpu.pprof
RUN CGO_ENABLED=1 GOOS=linux go build -o profiler ./cmd/profiler/ && \
    ./profiler && \
    ls -lh cpu.pprof

# --- Estágio Builder Go: API com CGO + PGO ---
FROM golang:1.24-bookworm AS go-builder
WORKDIR /app

# Copia arquivos do Go API
COPY go_api/go.mod ./
COPY go_api/main.go .
COPY go_api/engine ./engine

# Resolve e baixa dependências baseadas no código
RUN go mod tidy && go mod download

# Copia lib estática do Rust
COPY --from=rust-builder /build/rust_engine/target/release/librust_engine.a /usr/local/lib/

# Copia o perfil PGO coletado
COPY --from=pgo-collector /app/cpu.pprof ./default.pgo

# Compilação Go vinculando a lib Rust via CGO, com PGO ativado e AVX2 (v3)
RUN CGO_ENABLED=1 GOOS=linux GOAMD64=v3 go build -pgo=default.pgo -ldflags="-s -w" -o rinha-api .

# --- Estágio Final: Imagem de Produção Enxuta ---
FROM gcr.io/distroless/cc-debian12:latest
WORKDIR /app

# Artefatos finais
COPY --from=go-builder /app/rinha-api .
COPY --from=rust-builder /build/rust_engine/dataset.bin ./
COPY resources/normalization.json ./resources/
COPY resources/mcc_risk.json ./resources/

# Configurações de runtime
ENV GOMAXPROCS=1

CMD ["./rinha-api"]
