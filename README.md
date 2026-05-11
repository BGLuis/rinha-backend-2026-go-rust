# Rinha de Backend 2026 - Go/Rust Hybrid Architecture

## 🚀 Visão Geral
Este projeto foi desenvolvido para a **Rinha de Backend 2026**, focando em detecção de fraudes com performance extrema, baixa latência e uso eficiente de recursos. A arquitetura é totalmente customizada, combinando um **Load Balancer em Rust (io_uring)**, uma **API em Go (fasthttp)** e um **Motor de Busca em Rust (SIMD)**.

## 🏗️ Arquitetura e Decisões de Design

A solução utiliza uma topologia de Camada 4 otimizada para o kernel Linux, eliminando overheads de rede tradicionais.

### 1. Custom Rust Load Balancer (io_uring)
- **Tecnologia**: Implementado em Rust utilizando a interface **`io_uring`** do Linux para I/O assíncrono.
- **Single-threaded Reactor**: Loop de eventos ultra-eficiente que gerencia milhares de conexões simultâneas com syscalls mínimas via SQEs/CQEs.
- **Unix Sockets IPC**: O balanceador comunica-se com as APIs via **Unix Domain Sockets** em `/tmp/sockets`. Isso evita toda a sobrecarga da stack TCP/IP (checksums, ACKs, retransmissões), permitindo latências próximas de zero no tráfego interno.

### 2. Go API (Networking & Orchestration)
- **fasthttp**: Servidor HTTP otimizado para evitar alocações, permitindo lidar com o tráfego da Rinha com baixa pressão no GC.
- **Zero-Allocation Parsing**: Uso de `jsonparser` para extrair dados sem instanciar objetos.
- **CGO Interop**: Delegação da busca para a engine Rust via ponteiros diretos (`unsafe.Pointer`).

### 3. Rust Engine (Vector Search Core)
- **Inverted File Index (IVF)**: Dataset de 3 milhões de vetores organizado em clusters para busca aproximada.
- **Int16 SIMD Quantization**: Quantização para inteiros de 16 bits, processando 16 dimensões simultaneamente via **AVX2/FMA**.
- **Memory Mapping (mmap)**: O dataset indexado é carregado via `mmap`. Como o arquivo é mapeado como **`MAP_SHARED`**, o kernel Linux garante que as duas instâncias da API compartilhem as **mesmas páginas de memória física** para o dataset, economizando ~280MB de RAM.

## ⚙️ Otimizações de Infraestrutura (Docker & OS)

Configurações críticas aplicadas para garantir estabilidade sob carga:

### 1. Estratégia de Warmup
- **Por que?**: O serviço de `warmup` executa 48 rodadas de requisições antes do início dos testes oficiais.
- **Objetivo**: Forçar o **"pre-faulting"** das páginas de memória mapeadas via `mmap`, aquecer as caches de página do kernel e preencher os buffers de rede. Isso evita picos de latência (cold start) nas primeiras requisições do teste de carga.

### 2. Ajustes de Ulimits
- **`nofile: 65535`**: Aumenta o limite de descritores de arquivo. Essencial para suportar a alta concorrência e o grande volume de conexões simultâneas entre LB, APIs e Clientes.
- **`memlock: -1`**: Permite que o processo bloqueie memória RAM ilimitada. Isso é usado para garantir que o dataset mapeado via `mmap` não seja enviado para o swap pelo kernel, mantendo-o sempre em memória física ultra-rápida.

### 3. Sockets em tmpfs
- **Configuração**: O volume `/tmp/sockets` é montado como **`tmpfs`** (RAM disk).
- **Vantagem**: Garante que o arquivo de socket exista apenas em memória, eliminando qualquer I/O de disco rotacional ou SSD durante a comunicação IPC entre o Load Balancer e as APIs.

## 🐳 Distribuição de Recursos

| Serviço | CPU | Memória | Função |
| :--- | :--- | :--- | :--- |
| **LB (Rust)** | 0.12 | 40MB | Balanceamento via io_uring & Sockets |
| **API 1 (Go/Rust)** | 0.42 | 150MB | API + Motor de Busca Vetorial |
| **API 2 (Go/Rust)** | 0.42 | 150MB | API + Motor de Busca Vetorial |
| **Warmup** | 0.04 | 10MB | Pre-heat do kernel e buffers |

## 📈 Dinâmicas de Detecção (@docs/**)
- **14 Dimensões**: Implementação estrita das regras de normalização de `DETECTION_RULES.md`.
- **K-NN (k=5)**: Classificação baseada nos 5 vizinhos mais próximos.

## 🛠️ Comandos Úteis
```bash
make build # Build das imagens
make smoke # Teste de contrato
make test  # k6 Load Test
```
