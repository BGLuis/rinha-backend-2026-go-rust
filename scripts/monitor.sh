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
# Tenta aguardar o container warmup sair (docker compose wait existe na v2)
WARMUP_EXIT_CODE=$(docker compose $COMPOSE_ARGS wait warmup | awk '{print $NF}' || echo "failed")
END_WARMUP=$(date +%s)
DURATION_WARMUP=$((END_WARMUP - START_WARMUP))

if [ "$WARMUP_EXIT_CODE" = "0" ]; then
    echo "✅ Warmup finalizado com sucesso em ${DURATION_WARMUP}s."
else
    echo "⚠️ Warmup finalizado com erro ou timeout (Exit Code: $WARMUP_EXIT_CODE) após ${DURATION_WARMUP}s."
    echo "--- Últimas 5 linhas do log de Warmup ---"
    docker compose $COMPOSE_ARGS logs warmup --tail 5
fi

echo "📊 Monitoramento iniciado em $(date)" > "$LOG_FILE"

# Função para monitorar containers em background
monitor_containers() {
  while true; do
    # Obtém IDs de todos os containers da stack atual
    CONTAINERS=$(docker compose $COMPOSE_ARGS ps -q)
    if [ -n "$CONTAINERS" ]; then
      # Coleta métricas de CPU, Memória, Rede e Disco
      docker stats --no-stream --format "container={{.Name}},cpu={{.CPUPerc}},mem={{.MemUsage}},mem_perc={{.MemPerc}},net={{.NetIO}},block={{.BlockIO}}" $CONTAINERS >> "$LOG_FILE" 2>/dev/null || true
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

# --- Diagnósticos de Erros e Saúde ---
echo "🔍 Diagnósticos de Saúde e Logs:" >> "$STATS_FILE"

# Verifica se containers morreram
EXITED_CONTAINERS=$(docker compose $COMPOSE_ARGS ps -a --format "{{.Name}}: {{.Status}}" | grep -E "Exited|Dead" || true)
if [ -n "$EXITED_CONTAINERS" ]; then
    echo "🚨 ATENÇÃO: Os seguintes containers finalizaram inesperadamente:" >> "$STATS_FILE"
    echo "$EXITED_CONTAINERS" >> "$STATS_FILE"
else
    echo "✅ Todos os containers principais permaneceram ativos." >> "$STATS_FILE"
fi

# Scrape de erros nos logs
echo "--- Resumo de Erros Encontrados nos Logs (Load Balancer & APIs) ---" >> "$STATS_FILE"
ERROR_LOGS=$(docker compose $COMPOSE_ARGS logs --tail 1000 | grep -Ei "error|panic|fatal|out of memory|oom-kill" | grep -v "warmup" | tail -n 10 || true)
if [ -n "$ERROR_LOGS" ]; then
    echo "$ERROR_LOGS" >> "$STATS_FILE"
else
    echo "Nenhum erro crítico (panic/oom) detectado nos logs recentes." >> "$STATS_FILE"
fi
echo "--------------------------------------------------------" >> "$STATS_FILE"

# --- Integração do Score Final ---
if [ -f "test/results.json" ]; then
    echo "🏆 Score Final (Extraído de test/results.json):" >> "$STATS_FILE"
    # Extrai campos específicos do JSON usando python (já que costuma estar presente em distros linux) ou grep/sed simples
    # Vou usar awk/sed simples para não depender de ferramentas extras
    grep -E "\"p99\"|\"failure_rate\"|\"final_score\"" test/results.json | sed 's/[",]//g' >> "$STATS_FILE" || true
    echo "--------------------------------------------------------" >> "$STATS_FILE"
fi

# Exibe o resultado final resumido no console
cat "$STATS_FILE"

echo "🧹 Desativando stack..."
docker compose $COMPOSE_ARGS down

echo "✅ Concluído! Resultados salvos em $STATS_FILE e $LOG_FILE"
