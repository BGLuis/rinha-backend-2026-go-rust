FROM rust:1.77-slim-bookworm AS rust-builder
WORKDIR /build/rust_engine
COPY rust_engine/ .
RUN RUSTFLAGS="-C target-feature=+avx2,+fma -C opt-level=3 -C lto=fat -C codegen-units=1 -C embed-bitcode=yes" cargo build --release

FROM golang:1.22-bookworm AS dataset-builder
WORKDIR /app
COPY resources/ ./resources/
COPY pre_processor/main.go ./pre_processor/
RUN go run pre_processor/main.go

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
COPY --from=dataset-builder /app/dataset.bin .
COPY resources/normalization.json ./resources/
COPY resources/mcc_risk.json ./resources/

ENV DATASET_PATH=/app/dataset.bin

CMD ["./rinha-api"]
