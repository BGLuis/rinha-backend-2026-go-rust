# Roadmap de Melhorias para Pontuação Máxima

A pontuação atual (~2.670 pts) já é muito alta, mas aqui estão os passos técnicos para buscar os **6.000 pontos (p99 < 1ms e 100% de acerto)**.

## 1. Redução de Latência (p99 < 1ms)
O objetivo é ganhar os +3000 pontos de performance.

- **Ajuste de nprobe**: Atualmente usamos `nprobe=8`. Reduzir para `nprobe=2` ou `nprobe=4` reduzirá o tempo de busca no Rust pela metade. 
- **Product Quantization (PQ)**: Em vez de guardar o vetor completo de `i16`, podemos comprimir o vetor em sub-espaços. Isso permite que mais vetores caibam no cache **L1** da CPU, acelerando a busca em 10x.
- **Substituir CGO por Rust puro**: Embora o CGO seja rápido, ele tem um overhead de ~50ns. Mudar o servidor HTTP para Rust (usando `Axum` ou `Actix` com `Monoio/io_uring`) eliminaria esse salto entre linguagens.
- **Ajuste de Afinidade de CPU**: Configurar as instâncias para rodarem em threads específicas (se permitido pelo Docker) para evitar trocas de contexto de hardware.

## 2. Aumento de Acurácia (Detecção)
O objetivo é zerar Falsos Negativos e Positivos.

- **IVF Refinado**: Aumentar o número de clusters de 1024 para 4096. Isso cria células de Voronoi menores e mais precisas.
- **Re-ranking**: Buscar no IVF usando uma versão comprimida (rápida) e, para os top 50 candidatos, refazer o cálculo usando o vetor original (`f32`) para precisão absoluta antes de dar o veredito final.
- **VP-Tree Local**: Em vez de força bruta dentro de cada cluster do IVF, construir uma VP-Tree minúscula dentro de cada cluster para que a busca local seja $O(\log \text{cluster\_size})$.

## 3. Micro-otimizações de Memória
- **Huge Pages**: Configurar o Linux/Docker para usar Huge Pages no `mmap`. Isso reduz o overhead de tradução de endereços virtuais (TLB misses), o que pode baixar a latência em mais 5-10%.
- **Padding de Cache Line**: Garantir que as estruturas de dados no Rust estejam alinhadas com o tamanho da linha de cache da CPU (64 bytes) para evitar *False Sharing*.

## 4. Estratégia de Jogo (Game Theory)
- **Tunning do Threshold**: O threshold de aprovação é `0.6`. Analisar se o arredondamento de floats no Go está causando perdas de precisão limítrofes e ajustar a quantização para minimizar esse erro.
- **Fail Fast**: O Circuit Breaker atual está em 1200ms. Experimentar baixar para 800ms. Responder "Legítimo" rápido é estatisticamente melhor do que tentar calcular no limite do tempo e correr o risco de um timeout imprevisível.
