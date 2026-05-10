FROM rust:1.82-slim-bookworm AS rust-builder
WORKDIR /build/rust_engine
COPY rust_engine/ .
RUN RUSTFLAGS="-C target-feature=+avx2,+fma" cargo build --release

FROM rust:1.82-slim-bookworm AS dataset-builder
WORKDIR /app
COPY resources/ ./resources/
COPY rust_engine/ ./rust_engine/
WORKDIR /app/rust_engine
RUN cargo run --release --bin build_index

FROM golang:1.22-bookworm AS go-builder
WORKDIR /app
COPY go_api/go.mod ./
COPY go_api/main.go .
RUN go mod tidy
RUN go mod download
COPY --from=rust-builder /build/rust_engine/target/release/librust_engine.a /usr/local/lib/
RUN go build -ldflags="-s -w" -o rinha-api main.go

FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=go-builder /app/rinha-api .
COPY --from=dataset-builder /app/rust_engine/dataset.bin .
COPY resources/normalization.json ./resources/
COPY resources/mcc_risk.json ./resources/

ENV DATASET_PATH=/app/dataset.bin

CMD ["./rinha-api"]
