#!/bin/bash
set -e

# Configurações de arquivos
COMPOSE_FILE=${1:-docker-compose.yml}
LOG_FILE="resource_usage.log"
STATS_FILE="test_stats.log"

echo "🚀 Iniciando stack usando $COMPOSE_FILE..."
docker compose -f "$COMPOSE_FILE" up -d

echo "⏳ Aguardando warmup e estabilização (15s)..."
sleep 15

echo "📊 Monitoramento iniciado em $(date)" > "$LOG_FILE"

# Função para monitorar containers em background
monitor_containers() {
  while true; do
    CONTAINERS=$(docker compose -f "$COMPOSE_FILE" ps -q)
    if [ -n "$CONTAINERS" ]; then
      # Coleta métricas básicas de CPU e Memória
      docker stats --no-stream --format "container={{.Name}},cpu={{.CPUPerc}},mem={{.MemUsage}},mem_perc={{.MemPerc}}" $CONTAINERS >> "$LOG_FILE" 2>/dev/null || true
    fi
    sleep 2
  done
}

# Inicia monitoramento em background
monitor_containers &
MONITOR_PID=$!

echo "🔥 Iniciando teste de carga (make test)..."
# Executa os testes mas não interrompe o script em caso de falha (para limpar os recursos)
make test || echo "⚠️ Os testes retornaram erro, mas continuarei para gerar as estatísticas."

echo "🛑 Teste de carga finalizado. Parando monitoramento..."
kill $MONITOR_PID
wait $MONITOR_PID 2>/dev/null || true

echo "📈 Gerando estatísticas em $STATS_FILE..."
echo "--- Estatísticas de Uso de Recursos (Média e Máximo) ---" > "$STATS_FILE"

# Processamento dos logs via awk para calcular médias e picos
awk -F',' '
  /container=/ {
    split($1, a, "="); name=a[2];
    split($2, b, "="); cpu=b[2]; sub("%", "", cpu);
    split($4, c, "="); mem=c[2]; sub("%", "", mem);
    
    if (cpu != "") {
      cpu_sum[name] += cpu;
      if (cpu > cpu_max[name]) cpu_max[name] = cpu;
      mem_sum[name] += mem;
      if (mem > mem_max[name]) mem_max[name] = mem;
      count[name]++;
    }
  }
  END {
    for (name in count) {
      if (count[name] > 0) {
        printf "Container: %-25s | CPU Avg: %6.2f%% (Max: %6.2f%%) | Mem Avg: %6.2f%% (Max: %6.2f%%)\n", 
          name, cpu_sum[name]/count[name], cpu_max[name], mem_sum[name]/count[name], mem_max[name]
      }
    }
  }
' "$LOG_FILE" | sort >> "$STATS_FILE"

echo "--------------------------------------------------------" >> "$STATS_FILE"

# Exibe o resultado final no console
cat "$STATS_FILE"

echo "🧹 Desativando stack..."
docker compose -f "$COMPOSE_FILE" down

echo "✅ Concluído! Resultados salvos em $STATS_FILE e $LOG_FILE"
