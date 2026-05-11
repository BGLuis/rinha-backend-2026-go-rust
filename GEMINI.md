# Rinha de Backend 2026 - Go/Rust Hybrid Architecture

## Arquitetura do Projeto
Este projeto utiliza uma abordagem híbrida focada em performance extrema para o desafio de detecção de fraudes.

- **Load Balancer (Rust)**: Proxy reverso customizado escrito em Rust utilizando **`io_uring`** para I/O assíncrono e baixo overhead de CPU.
- **Go API**: Servidor web usando `fasthttp` e `jsonparser` com `EachKey` para garantir zero-alocação e processamento em única passada no caminho crítico das requisições.
- **Rust Engine**: Motor de busca vetorial compilado como biblioteca estática. Utiliza o algoritmo **Inverted File Index (IVF)** com clusterização **K-Means**.
- **Otimizações de Busca**:
    - **BBox Pruning**: Técnica de poda por bounding boxes para clusters.
    - **Custom Binary Format**: O dataset é indexado em um formato binário próprio (assinatura `0x4E495452`).
- **Interoperabilidade**: Comunicação via **CGO** passando ponteiros de memória (`unsafe.Pointer`) para evitar cópias.
- **Otimização de Memória**: Utiliza `mmap` compartilhado para carregar o dataset de 3M vetores. **O uso de `MAP_SHARED` é obrigatório** para permitir que múltiplas instâncias compartilhem a mesma memória física.
- **Aceleração de Hardware**: Compilação em Rust com flags para **AVX2** e **FMA**. Uso de **Int16 SIMD Quantization**.

## Convenções de Desenvolvimento & Infra
- **Zero-Allocation**: Nunca use `json.Marshal` ou `json.Unmarshal` no handler `/fraud-score`.
- **Unix Sockets**: A comunicação entre o Load Balancer e a API deve ser via **Unix Domain Sockets** em `/tmp/sockets`.
- **tmpfs para Sockets**: O diretório de sockets deve ser montado como **`tmpfs`** para evitar latência de disco.
- **Ulimits Mandatórios**:
    - `nofile: 65535` para suportar alta concorrência.
    - `memlock: -1` para permitir o bloqueio de páginas do dataset em RAM física via `mmap`.
- **Estratégia de Warmup**: O sistema deve passar por um processo de aquecimento (min. 48 rodadas) para pre-faulting de memória e preenchimento de caches do kernel.

## Comandos Úteis (Makefile)
- `make build`: Constrói as imagens.
- `make smoke`: Teste rápido de fumaça.
- `make test`: Teste de carga completo.

## Estratégia de Branches
- `main`: Código-fonte completo e limpo.
- `submission`: Apenas artefatos Docker e configurações necessárias para o deploy oficial.
