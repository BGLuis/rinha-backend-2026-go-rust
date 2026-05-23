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
- **Batching UDS (IPC)**: O proxy Rust repassa conexões (FDs) para a API Go em blocos usando `SCM_RIGHTS`, cortando chamadas de sistema (syscalls) na barreira Go-Rust em mais de 90%.
- **Otimizações Extremas da API Go**:
    - **Aritmética de Epoch Direta**: Substituição de parsers padrão (`time.Date`) por matemática escalar nativa para evitar *Garbage Collection* e *locks* de *Timezone*.
    - **Routing sem GC**: Roteamento HTTP via variáveis zero-copy e ponteiros diretos (`crlf` globais).

## Convenções de Desenvolvimento & Infra
- **Zero-Allocation**: Nunca use `json.Marshal` ou `json.Unmarshal` no handler `/fraud-score`.
- **Nenhum pacote `time` nativo**: Cálculos temporais no fluxo quente devem usar a função `fastUnix` escalar, preservando microssegundos preciosos.
- **Unix Sockets & Batching**: A comunicação entre o Load Balancer e a API deve ser via **Unix Domain Sockets** montados em `/tmp/sockets` (diretório `tmpfs`). O LB envia *File Descriptors* em massa em lotes (chunks) de até 16.
- **Ulimits Mandatórios**:
    - `nofile: 65535` para suportar alta concorrência.
    - `memlock: -1` para permitir o bloqueio de páginas do dataset em RAM física via `mmap`.
- **Estratégia de Warmup**: O sistema deve passar por um processo de aquecimento (min. 48 rodadas) para pre-faulting de memória e preenchimento de caches do kernel.

## Comandos Úteis (Makefile)
- `make build`: Constrói as imagens Docker da stack.
- `make restart-clean`: Reinicia a stack limpando estados e aguarda o warmup.
- `make smoke`: Teste de fumaça (valida contrato e conectividade).
- `make test`: **Teste Oficial da Rinha** (900 req/s, 120s).
- `make test-precision`: Mede acurácia (FP/FN) sem ruído de throughput.
- `make test-thermal`: Testa distribuição de clusters IVF e cache.
- `make test-sustained`: Carga constante de 800 req/s por 6 minutos.
- `make test-saturation`: Encontra o ponto de ruptura do sistema.
- `make test-spike`: Avalia tempo de recuperação após sobrecarga.
- `make heavy-test`: Stress extremo com picos de até 8000 req/s.
- `make all-tests`: Executa toda a bateria de testes em sequência.

## Testes e Validação
A stack é validada por uma bateria de testes k6 customizados que garantem a performance sob condições extremas, além do teste oficial.

### 1. Funcionalidade e Contrato
- **`smoke.js`**: Valida se o endpoint `/fraud-score` respeita o contrato JSON e retorna `200 OK`.

### 2. Acurácia e Engine Vetorial
- **`precision_probe.js`**: Executa o dataset completo com baixa concorrência (4 VUs). Essencial para calibrar o `nprobe` do IVF e validar se o pré-processamento binário não introduziu distorções nos vetores.
- **`cache_thermal.js`**: Força o acesso a fatias distintas do dataset simultaneamente. Valida se o sistema mantém o `p99` baixo mesmo quando clusters pesados do K-Means são solicitados em paralelo.

### 3. Performance e Estresse
- **`test.js` (Oficial)**: Alvo principal da competição. O sistema deve manter `p99 <= 1ms` até 900 req/s.
- **`stress_sustained.js`**: Monitora a degradação ao longo de 6 minutos. Identifica problemas de pressão de GC no Go ou eviction de páginas `mmap` pelo SO.
- **`saturation_finder.js`**: Escalonamento granular para identificar o limite real de throughput (req/s) antes da explosão da latência.
- **`spike_recovery.js`**: Simula um "tsunami" de requisições seguido de queda brusca para medir o tempo que o kernel e a API levam para drenar os buffers e voltar ao baseline.
- **`heavyTests.js`**: Teste de estresse agressivo (até 8000 req/s) para garantir a estabilidade do `io_uring` e dos Unix Sockets sob pressão máxima.

## Estratégia de Branches
- `main`: Código-fonte completo e limpo.
- `submission`: Apenas artefatos Docker e configurações necessárias para o deploy oficial.
