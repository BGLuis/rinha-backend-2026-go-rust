# Melhores Estratégias - Rinha de Backend 2026

Compilado de otimizações extraídas dos 10 melhores projetos da competição.

## 1. Networking e Infraestrutura (Zero-Overhead)
- **Unix Domain Sockets (UDS) em tmpfs**: Utilizar UDS em vez de TCP/IP para comunicação entre Nginx/HAProxy e as APIs. Montar em `/tmp` como `tmpfs` para eliminar latência de rede e disco.
- **Nginx `res.cork()` / `writev`**: Agrupar headers e corpo da resposta em um único envio ao kernel (syscall) para reduzir overhead.
- **Imagens `scratch` ou `distroless`**: Reduzir a imagem final ao mínimo absoluto, contendo apenas o binário estático.
- **Afinidade de CPU**: Se possível, fixar processos em cores específicos para evitar context switching.

## 2. Processamento de Requisições (Zero-Allocation)
- **Parser JSON Manual**: Não usar `json.Unmarshal` ou `JSON.parse`. Localizar chaves via busca de bytes (`memchr`) e extrair valores diretamente do buffer.
- **Reuso de Objetos (Pools)**: Pré-alocar structs de payload e vetores e reutilizá-los em cada requisição para evitar pressão no GC.
- **Respostas Estáticas**: Como só existem 6 resultados possíveis (0, 1, 2, 3, 4, 5 fraudes), pré-computar os JSONs de resposta como `&'static [u8]` e retorná-los diretamente.

## 3. Motor de Busca Vetorial (SIMD & Algoritmos)
- **Quantização i16**: Converter os 14 floats (0.0 a 1.0) para `i16` com escala (ex: 8192 ou 10000). Reduz memória pela metade e acelera cálculos SIMD.
- **SIMD AVX2/FMA**:
    - Usar `_mm256_sub_epi16` e `_mm256_madd_epi16` para cálculos de distância com inteiros quantizados.
    - Usar `_mm256_fmadd_ps` para acumular produtos em ponto flutuante.
- **Algoritmo IVF (Inverted File Index)**:
    - Agrupar 3M de vetores em clusters (centroids) usando K-Means.
    - Buscar apenas nos `nprobe` clusters mais próximos.
    - **Adaptive nprobe**: Usar `nprobe` baixo para casos óbvios e expandir para `nprobe` alto apenas em casos "borderline" (incerteza na zona de 0.6).
- **Early Exit (Pruning)**: Durante o scan de um cluster, se a distância parcial (ex: primeiras 8 dimensões) já for maior que o 5º melhor vizinho atual, descartar o vetor imediatamente.
- **Prefetch de Cache**: Usar `_mm_prefetch` para carregar blocos de memória no cache L1 antes de serem processados.
- **Memory Alignment**: Garantir alinhamento de 32 ou 64 bytes para todas as estruturas de dados para máxima eficiência de instruções AVX.

## 4. Estratégia de Dados
- **Build-time Processing**: Decomprimir `references.json.gz`, gerar o índice (clusters), quantizar e salvar em um formato binário proprietário (`index.bin`) durante o build da imagem Docker.
- **Embed no Binário**: Em Rust, usar `include_bytes!("index.bin")` para embutir o dataset diretamente no binário, eliminando I/O em runtime.
- **Warmup no Startup**: Executar centenas de buscas dummy ao iniciar a API para aquecer os caches de CPU e o branch predictor.

## 5. Arquitetura Híbrida Go/Rust
- **Go para I/O**: Usar `fasthttp` ou `net.ListenUnix` para gerenciar conexões.
- **Rust para Math**: Implementar a lógica de busca vetorial em Rust e chamar via CGO ou FFI.
- **Ponteiros de Memória**: Passar apenas ponteiros do Go para o Rust para evitar cópia de dados.
