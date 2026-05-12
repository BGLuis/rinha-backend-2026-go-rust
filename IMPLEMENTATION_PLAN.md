# Plano de Implementação - Rinha de Backend 2026

Este documento detalha os passos para implementar a solução híbrida Go/Rust com performance extrema, baseada nas melhores práticas dos 10 melhores projetos da competição.

## 1. Fase de Preparação (Data Engineering)
- **Script de Pré-processamento (Rust)**:
    - Criar um utilitário em `rust_engine/bin/indexer.rs`.
    - Ler `resources/references.json.gz`.
    - Executar K-Means para gerar ~4096 clusters (centroids).
    - Quantizar vetores para `i16` (escala 8192).
    - Salvar o arquivo `index.bin` com layout SoA (Structure of Arrays) por cluster para facilitar o uso de SIMD.
- **Integração no Build**:
    - Configurar o `Dockerfile` para rodar o indexador durante o build e embutir o `index.bin` no binário final via `include_bytes!`.

## 2. Implementação do Rust Engine (Busca Vetorial)
- **Cálculo de Distância SIMD**:
    - Implementar `distance_avx2_i16` usando `_mm256_sub_epi16` e `_mm256_madd_epi16`.
- **Algoritmo IVF**:
    - Passo 1: Encontrar os 8 centroids mais próximos.
    - Passo 2: Scan nos vetores desses 8 clusters.
    - Implementar **Early Exit**: Se a distância parcial exceder a distância do 5º vizinho, parar.
- **Interface CGO/FFI**:
    - Expor uma função `fraud_score(query_vector: *const f32) -> i32` para ser chamada pelo Go.

## 3. Implementação da Go API (I/O e Orquestração)
- **Servidor HTTP**:
    - Usar `fasthttp` configurado para escutar em Unix Domain Sockets (`/tmp/api.sock`).
- **Parser JSON Zero-Allocation**:
    - Usar `jsonparser.EachKey` para percorrer os bytes do payload sem criar objetos intermediários.
    - Mapear os campos para o vetor de 14 dimensões diretamente.
- **Orquestração**:
    - Chamar a função do Rust Engine passando o vetor quantizado.
    - Retornar uma das 6 respostas JSON estáticas (pré-alocadas como `[]byte`).

## 4. Configuração de Infraestrutura
- **Nginx**:
    - Configurar como Load Balancer usando `upstream` via Unix Sockets.
    - Habilitar `keepalive` e otimizações de buffer.
- **Docker Compose**:
    - Aplicar limites rígidos de CPU (0.45 para cada API, 0.10 para Nginx) e Memória (160MB para cada API).
    - Montar volumes `tmpfs` para os sockets.

## 5. Validação e Benchmarking
- **Warmup**: Implementar um loop no `main.go` que faz 500 chamadas ao motor de busca antes de responder 200 no `/ready`.
- **Testes**:
    - `make smoke`: Validar corretude com `example-payloads.json`.
    - `make test`: Executar k6 completo e monitorar p99.

## Próximos Passos (Instruções para a IA)
1. Inicie pela implementação do **Indexador Rust** em `rust_engine/src/bin/indexer.rs`.
2. Em seguida, implemente as funções de **intrinsics AVX2** no `rust_engine/src/lib.rs`.
3. Refatore a **Go API** em `go_api/main.go` para usar `fasthttp` e `jsonparser` com suporte a UDS.
4. Finalize com o ajuste do `Dockerfile` multi-stage.
