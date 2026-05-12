#!/bin/bash
set -e

# Configurações de arquivos
OVERRIDE_FILE=$1
LOG_FILE="resource_usage.log"
STATS_FILE="test_stats.log"

# Define os argumentos do compose. Sempre usa o base, e opcionalmente um override.
COMPOSE_ARGS="-f docker-compose.yml"
if [ -n "$OVERRIDE_FILE" ] && [ "$OVERRIDE_FILE" != "docker-compose.yml" ]; then
    echo "🏗️ Usando override: $OVERRIDE_FILE"
    COMPOSE_ARGS="$COMPOSE_ARGS -f $OVERRIDE_FILE"
fi

echo "🚀 Iniciando stack..."
docker compose $COMPOSE_ARGS up -d

echo "⏳ Aguardando warmup e estabilização (15s)..."
sleep 15

echo "📊 Monitoramento iniciado em $(date)" > "$LOG_FILE"

# Função para monitorar containers em background
monitor_containers() {
  while true; do
    # Obtém IDs de todos os containers da stack atual
    CONTAINERS=$(docker compose $COMPOSE_ARGS ps -q)
    if [ -n "$CONTAINERS" ]; then
      docker stats --no-stream --format "container={{.Name}},cpu={{.CPUPerc}},mem={{.MemUsage}},mem_perc={{.MemPerc}}" $CONTAINERS >> "$LOG_FILE" 2>/dev/null || true
    fi
    sleep 2
  done
}

# Inicia monitoramento em background
monitor_containers &
MONITOR_PID=$!

echo "🔥 Iniciando teste de carga (make test)..."
make test || echo "⚠️ Os testes retornaram erro, mas continuarei para gerar as estatísticas."

echo "🛑 Teste de carga finalizado. Parando monitoramento..."
kill $MONITOR_PID
wait $MONITOR_PID 2>/dev/null || true

echo "📈 Gerando estatísticas em $STATS_FILE..."
echo "--- Estatísticas de Uso de Recursos (Média e Máximo) ---" > "$STATS_FILE"

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
cat "$STATS_FILE"

echo "🧹 Desativando stack..."
docker compose $COMPOSE_ARGS down

echo "✅ Concluído! Resultados salvos em $STATS_FILE e $LOG_FILE"
