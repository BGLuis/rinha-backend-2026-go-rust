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

echo "⏳ Aguardando warmup dinâmico concluir..."
START_WARMUP=$(date +%s)
WARMUP_EXIT_CODE=$(docker compose $COMPOSE_ARGS wait warmup | awk '{print $NF}' || echo "failed")
END_WARMUP=$(date +%s)
DURATION_WARMUP=$((END_WARMUP - START_WARMUP))

if [ "$WARMUP_EXIT_CODE" = "0" ]; then
    echo "✅ Warmup finalizado com sucesso em ${DURATION_WARMUP}s."
else
    echo "⚠️ Warmup finalizado com erro ou timeout (Exit Code: $WARMUP_EXIT_CODE) após ${DURATION_WARMUP}s."
    docker compose $COMPOSE_ARGS logs warmup --tail 5
fi

echo "📊 Monitoramento iniciado em $(date)" > "$LOG_FILE"

monitor_containers() {
  while true; do
    CONTAINERS=$(docker compose $COMPOSE_ARGS ps -q)
    if [ -n "$CONTAINERS" ]; then
      # Formato robusto para evitar quebras de parsing
      docker stats --no-stream --format "{{.Name}},{{.CPUPerc}},{{.MemUsage}},{{.MemPerc}},{{.NetIO}},{{.BlockIO}}" $CONTAINERS >> "$LOG_FILE" 2>/dev/null || true
    fi
    sleep 2
  done
}

monitor_containers &
MONITOR_PID=$!

echo "🔥 Iniciando teste de carga (make test)..."
make test || echo "⚠️ Os testes retornaram erro, mas continuarei para gerar as estatísticas."

echo "🛑 Teste de carga finalizado. Parando monitoramento..."
kill $MONITOR_PID
wait $MONITOR_PID 2>/dev/null || true

echo "📈 Gerando estatísticas em $STATS_FILE..."
echo "--- Estatísticas de Uso de Recursos (Média e Máximo) ---" > "$STATS_FILE"

# Parsing robusto do CSV gerado pelo docker stats
awk -F',' '
  {
    name=$1;
    cpu=$2; sub("%", "", cpu);
    mem_p=$4; sub("%", "", mem_p);
    
    if (cpu ~ /^[0-9.]+$/) {
      cpu_sum[name] += cpu;
      if (cpu > cpu_max[name] || cpu_max[name] == "") cpu_max[name] = cpu;
      mem_sum[name] += mem_p;
      if (mem_p > mem_max[name] || mem_max[name] == "") mem_max[name] = mem_p;
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
echo "🔍 Diagnósticos de Saúde e Logs:" >> "$STATS_FILE"

EXITED_CONTAINERS=$(docker compose $COMPOSE_ARGS ps -a --format "{{.Name}}: {{.Status}}" | grep -E "Exited|Dead" || true)
if [ -n "$EXITED_CONTAINERS" ]; then
    echo "🚨 ATENÇÃO: Containers que finalizaram:" >> "$STATS_FILE"
    echo "$EXITED_CONTAINERS" >> "$STATS_FILE"
else
    echo "✅ Todos os containers permaneceram ativos." >> "$STATS_FILE"
fi

echo "--- Resumo de Erros nos Logs ---" >> "$STATS_FILE"
docker compose $COMPOSE_ARGS logs --tail 1000 | grep -Ei "error|panic|fatal|out of memory|oom-kill" | grep -v "warmup" | tail -n 10 >> "$STATS_FILE" || true
echo "--------------------------------------------------------" >> "$STATS_FILE"

if [ -f "test/results.json" ]; then
    echo "🏆 Score Final:" >> "$STATS_FILE"
    grep -E "\"p99\"|\"failure_rate\"|\"final_score\"" test/results.json | sed 's/[",]//g' >> "$STATS_FILE" || true
    echo "--------------------------------------------------------" >> "$STATS_FILE"
fi

cat "$STATS_FILE"
docker compose $COMPOSE_ARGS down
echo "✅ Concluído!"
