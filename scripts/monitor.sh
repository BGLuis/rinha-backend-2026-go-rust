#!/bin/bash
set -e

# Configurações de arquivos
OVERRIDE_FILE=$1
TEST_TARGET=${2:-test}
LOG_FILE="resource_usage.log"
STATS_FILE="test_stats.log"
REPORT_FILE="profile_report.log"

# Define os argumentos do compose. Sempre usa o base, e opcionalmente um override.
COMPOSE_ARGS="-f docker-compose.yml"
if [ -n "$OVERRIDE_FILE" ] && [ "$OVERRIDE_FILE" != "docker-compose.yml" ] && [ -f "$OVERRIDE_FILE" ]; then
    echo "🏗️  Usando override: $OVERRIDE_FILE"
    COMPOSE_ARGS="$COMPOSE_ARGS -f $OVERRIDE_FILE"
fi

echo "=========================================================="
echo "⚡ INICIANDO AMBIENTE DE TELEMETRIA & DIAGNÓSTICO AVANÇADO"
echo "=========================================================="

# 1. Verificação de suporte a eBPF no Kernel
EBPF_STATUS="Desabilitado (Ambiente LXC/Container sem acesso a tracefs)"
if [ -d "/sys/kernel/tracing" ] && [ -r "/sys/kernel/tracing/available_events" ]; then
    EBPF_STATUS="Ativo (Kernel tracefs disponível)"
elif [ -d "/sys/kernel/debug/tracing" ] && [ -r "/sys/kernel/debug/tracing/available_events" ]; then
    EBPF_STATUS="Ativo (Debugfs tracefs disponível)"
fi
echo "🔍 Kernel: $(uname -r) | Status eBPF Tracefs: $EBPF_STATUS"
echo "🔬 Mecanismo de Telemetria: cgroup v2 + ProcFS Deep Telemetry"

# 2. Inicialização dos Containers
echo ""
echo "🚀 Subindo stack com Docker Compose..."
docker compose $COMPOSE_ARGS up -d

echo "⏳ Aguardando stack responder (/ready)..."
START_WAIT=$(date +%s)
READY=0
for i in $(seq 1 60); do
    if curl -s -f http://localhost:9999/ready >/dev/null 2>&1; then
        READY=1
        break
    fi
    sleep 0.2
done

if [ "$READY" = "1" ]; then
    DURATION_WAIT=$(( $(date +%s) - START_WAIT ))
    echo "✅ Stack online e pronta para requisições em ${DURATION_WAIT}s."
else
    echo "⚠️  Stack demorou para responder ao /ready."
fi

# Aguarda container de warmup se estiver configurado e ativo
if docker compose $COMPOSE_ARGS config --services 2>/dev/null | grep -q "^warmup$"; then
    echo "⏳ Aguardando warmup dinâmico concluir..."
    docker compose $COMPOSE_ARGS wait warmup 2>/dev/null || true
fi

# 3. Mapeamento de Containers, Cgroups e PIDs do Host
declare -A CONTAINER_CGROUP
declare -A CONTAINER_PID
declare -A CG_THROTTLE_START
declare -A CG_THROTTLED_US_START
declare -A CG_PGFAULT_START
declare -A CG_PGMAJFAULT_START
declare -A PROC_VOL_START
declare -A PROC_NONVOL_START

CONTAINER_NAMES=""
CONTAINER_IDS=$(docker compose $COMPOSE_ARGS ps -q)

for cid in $CONTAINER_IDS; do
    cname=$(docker inspect --format '{{.Name}}' "$cid" 2>/dev/null | sed 's/^\///')
    if [ -n "$cname" ]; then
        CONTAINER_NAMES="$CONTAINER_NAMES $cname"
        full_cid=$(docker inspect --format '{{.Id}}' "$cid" 2>/dev/null || true)
        cgroup_path="/sys/fs/cgroup/system.slice/docker-${full_cid}.scope"
        if [ -d "$cgroup_path" ]; then
            CONTAINER_CGROUP["$cname"]="$cgroup_path"
            
            # Snapshots iniciais de CPU throttling
            if [ -f "$cgroup_path/cpu.stat" ]; then
                CG_THROTTLE_START["$cname"]=$(awk '/nr_throttled/ {print $2}' "$cgroup_path/cpu.stat" 2>/dev/null || echo 0)
                CG_THROTTLED_US_START["$cname"]=$(awk '/throttled_usec/ {print $2}' "$cgroup_path/cpu.stat" 2>/dev/null || echo 0)
            fi
            
            # Snapshots iniciais de Page Faults
            if [ -f "$cgroup_path/memory.stat" ]; then
                CG_PGFAULT_START["$cname"]=$(awk '/^pgfault / {print $2}' "$cgroup_path/memory.stat" 2>/dev/null || echo 0)
                CG_PGMAJFAULT_START["$cname"]=$(awk '/^pgmajfault / {print $2}' "$cgroup_path/memory.stat" 2>/dev/null || echo 0)
            fi
        fi
        
        # PIDs no Host para estatísticas de trocas de contexto
        main_pid=$(docker top "$cid" 2>/dev/null | awk 'NR==2 {print $2; exit}')
        if [ -n "$main_pid" ] && [ -f "/proc/$main_pid/status" ]; then
            CONTAINER_PID["$cname"]="$main_pid"
            v=$(awk '/voluntary_ctxt_switches:/ {print $2; exit}' "/proc/$main_pid/status" 2>/dev/null || echo 0)
            nv=$(awk '/nonvoluntary_ctxt_switches:/ {print $2; exit}' "/proc/$main_pid/status" 2>/dev/null || echo 0)
            PROC_VOL_START["$cname"]="${v:-0}"
            PROC_NONVOL_START["$cname"]="${nv:-0}"
        fi
    fi
done

# 4. Amostragem em Tempo Real com Docker Stats (1 amostra por segundo)
echo "📊 Iniciando captura de telemetria contínua..." > "$LOG_FILE"
rm -f test/results.json

monitor_containers() {
  while true; do
    CONTAINERS=$(docker compose $COMPOSE_ARGS ps -q 2>/dev/null || true)
    if [ -n "$CONTAINERS" ]; then
      docker stats --no-stream --format "{{.Name}},{{.CPUPerc}},{{.MemUsage}},{{.MemPerc}},{{.NetIO}},{{.BlockIO}}" $CONTAINERS >> "$LOG_FILE" 2>/dev/null || true
    fi
    sleep 1
  done
}

# Captura inicial imediata
CONTAINERS=$(docker compose $COMPOSE_ARGS ps -q 2>/dev/null || true)
if [ -n "$CONTAINERS" ]; then
  docker stats --no-stream --format "{{.Name}},{{.CPUPerc}},{{.MemUsage}},{{.MemPerc}},{{.NetIO}},{{.BlockIO}}" $CONTAINERS >> "$LOG_FILE" 2>/dev/null || true
fi

monitor_containers &
MONITOR_PID=$!

# 5. Execução do Teste de Carga
echo ""
echo "🔥 Executando teste de carga oficial (alvo: make $TEST_TARGET)..."
make "$TEST_TARGET" || echo "⚠️  Os testes retornaram erro, mas continuarei para gerar as estatísticas."

# 6. Finalização da Captura e Snapshots Finais
echo "🛑 Teste de carga finalizado. Coletando snapshots pós-teste..."
kill $MONITOR_PID 2>/dev/null || true
wait $MONITOR_PID 2>/dev/null || true

# 7. Relatório de Telemetria e Diagnóstico Avançado
{
  echo "=========================================================="
  echo "         RELATÓRIO DE TELEMETRIA E PERFORMANCE"
  echo "=========================================================="
  echo "📅 Data: $(date)"
  echo "🔍 eBPF / Kernel Tracefs: $EBPF_STATUS"
  echo ""
  echo "--- 1. USO DE RECURSOS (MÉDIA / MÁXIMO) ---"
  
  awk -F',' '
    {
      name=$1;
      cpu=$2; sub("%", "", cpu);
      mem_p=$4; sub("%", "", mem_p);
      mem_use=$3;
      
      if (cpu ~ /^[0-9.]+$/) {
        cpu_sum[name] += cpu;
        if (cpu > cpu_max[name] || cpu_max[name] == "") cpu_max[name] = cpu;
        mem_sum[name] += mem_p;
        if (mem_p > mem_max[name] || mem_max[name] == "") mem_max[name] = mem_p;
        last_mem[name] = mem_use;
        count[name]++;
      }
    }
    END {
      for (name in count) {
        if (count[name] > 0) {
          printf "Container: %-22s | CPU: %5.1f%% (Max: %5.1f%%) | RAM: %5.1f%% (Max: %5.1f%%)\n", 
            name, cpu_sum[name]/count[name], cpu_max[name], mem_sum[name]/count[name], mem_max[name]
        }
      }
    }
  ' "$LOG_FILE" | sort

  echo ""
  echo "--- 2. DIAGNÓSTICO DO KERNEL (CGROUP V2 & PROCFS) ---"
  for cname in $CONTAINER_NAMES; do
    cg="${CONTAINER_CGROUP[$cname]}"
    if [ -n "$cg" ] && [ -d "$cg" ]; then
      # CPU Throttling
      th_end=$(awk '/nr_throttled/ {print $2; exit}' "$cg/cpu.stat" 2>/dev/null || echo 0)
      th_us_end=$(awk '/throttled_usec/ {print $2; exit}' "$cg/cpu.stat" 2>/dev/null || echo 0)
      th_end=${th_end//[^0-9]/}; th_end=${th_end:-0}
      th_us_end=${th_us_end//[^0-9]/}; th_us_end=${th_us_end:-0}
      th_start=${CG_THROTTLE_START[$cname]:-0}; th_start=${th_start//[^0-9]/}; th_start=${th_start:-0}
      th_us_start=${CG_THROTTLED_US_START[$cname]:-0}; th_us_start=${th_us_start//[^0-9]/}; th_us_start=${th_us_start:-0}
      th_delta=$((th_end - th_start))
      th_ms_delta=$(( (th_us_end - th_us_start) / 1000 ))
      
      # Page Faults
      pf_end=$(awk '/^pgfault / {print $2; exit}' "$cg/memory.stat" 2>/dev/null || echo 0)
      pmajf_end=$(awk '/^pgmajfault / {print $2; exit}' "$cg/memory.stat" 2>/dev/null || echo 0)
      pf_end=${pf_end//[^0-9]/}; pf_end=${pf_end:-0}
      pmajf_end=${pmajf_end//[^0-9]/}; pmajf_end=${pmajf_end:-0}
      pf_start=${CG_PGFAULT_START[$cname]:-0}; pf_start=${pf_start//[^0-9]/}; pf_start=${pf_start:-0}
      pmajf_start=${CG_PGMAJFAULT_START[$cname]:-0}; pmajf_start=${pmajf_start//[^0-9]/}; pmajf_start=${pmajf_start:-0}
      pf_delta=$((pf_end - pf_start))
      pmajf_delta=$((pmajf_end - pmajf_start))
      
      # Peak Memory em MB
      mem_peak_bytes=$(cat "$cg/memory.peak" 2>/dev/null || echo 0)
      mem_peak_bytes=${mem_peak_bytes//[^0-9]/}; mem_peak_bytes=${mem_peak_bytes:-0}
      mem_peak_mb=$(awk -v b="$mem_peak_bytes" 'BEGIN { printf "%.1fMB", b / (1024*1024) }')
      
      # Context Switches
      pid="${CONTAINER_PID[$cname]}"
      vol_delta=0
      nonvol_delta=0
      if [ -n "$pid" ] && [ -f "/proc/$pid/status" ]; then
        vol_end=$(awk '/voluntary_ctxt_switches:/ {print $2; exit}' "/proc/$pid/status" 2>/dev/null || echo 0)
        nonvol_end=$(awk '/nonvoluntary_ctxt_switches:/ {print $2; exit}' "/proc/$pid/status" 2>/dev/null || echo 0)
        vol_end=${vol_end//[^0-9]/}; vol_end=${vol_end:-0}
        nonvol_end=${nonvol_end//[^0-9]/}; nonvol_end=${nonvol_end:-0}
        vol_start=${PROC_VOL_START[$cname]:-0}; vol_start=${vol_start//[^0-9]/}; vol_start=${vol_start:-0}
        nonvol_start=${PROC_NONVOL_START[$cname]:-0}; nonvol_start=${nonvol_start//[^0-9]/}; nonvol_start=${nonvol_start:-0}
        vol_delta=$((vol_end - vol_start))
        nonvol_delta=$((nonvol_end - nonvol_start))
      fi
      
      echo "[$cname]"
      echo "  • Memória de Pico: $mem_peak_mb"
      if [ "$th_delta" -gt 0 ]; then
        echo "  • ⚠️  CFS CPU Throttled: $th_delta períodos (${th_ms_delta}ms perdidos por throttle)"
      else
        echo "  • ✅ CFS CPU Throttled: 0 (sem gargalo de quota de CPU)"
      fi
      if [ "$pmajf_delta" -eq 0 ]; then
        echo "  • ✅ Major Page Faults: 0 (mmap MAP_SHARED + mlock intactos em RAM física)"
      else
        echo "  • ⚠️  Major Page Faults: $pmajf_delta (houve I/O de disco para páginas de memória)"
      fi
      echo "  • Minor Page Faults: $pf_delta"
      echo "  • Context Switches: Voluntary: $vol_delta | Involuntary: $nonvol_delta"
      echo ""
    fi
  done

  echo "--- 3. SAÚDE DOS CONTAINERS & LOGS ---"
  EXITED_CONTAINERS=$(docker compose $COMPOSE_ARGS ps -a --format "{{.Name}}: {{.Status}}" | grep -E "Exited|Dead" || true)
  if [ -n "$EXITED_CONTAINERS" ]; then
    echo "🚨 ATENÇÃO: Containers que finalizaram precocemente:"
    echo "$EXITED_CONTAINERS"
  else
    echo "✅ Todos os containers permaneceram ativos e estáveis."
  fi

  ERROR_LOGS=$(docker compose $COMPOSE_ARGS logs --tail 1000 | grep -Ei "error|panic|fatal|out of memory|oom-kill" | grep -v "warmup" | tail -n 5 || true)
  if [ -n "$ERROR_LOGS" ]; then
    echo "⚠️  Erros recentes nos logs:"
    echo "$ERROR_LOGS"
  else
    echo "✅ Zero erros críticos nos logs (nenhum panic, OOM ou fatal error)."
  fi

  echo ""
  echo "--- 4. PONTUAÇÃO & ACURÁCIA (RINHA DE BACKEND) ---"
  if [ -f "test/results.json" ]; then
    p99=$(grep -E '"p99":' test/results.json 2>/dev/null | head -n1 | sed -E 's/.*"p99":[[:space:]]*"?([^",]+)"?.*/\1/' || echo "-")
    final_score=$(grep -E '"final_score":' test/results.json 2>/dev/null | head -n1 | sed -E 's/.*"final_score":[[:space:]]*"?([^",]+)"?.*/\1/' || echo "-")
    fail_rate=$(grep -E '"failure_rate":' test/results.json 2>/dev/null | head -n1 | sed -E 's/.*"failure_rate":[[:space:]]*"?([^",]+)"?.*/\1/' || echo "-")
    fp=$(grep -E '"false_positive_detections":' test/results.json 2>/dev/null | head -n1 | sed -E 's/.*"false_positive_detections":[[:space:]]*"?([^",]+)"?.*/\1/' || echo "0")
    fn=$(grep -E '"false_negative_detections":' test/results.json 2>/dev/null | head -n1 | sed -E 's/.*"false_negative_detections":[[:space:]]*"?([^",]+)"?.*/\1/' || echo "0")
    tp=$(grep -E '"true_positive_detections":' test/results.json 2>/dev/null | head -n1 | sed -E 's/.*"true_positive_detections":[[:space:]]*"?([^",]+)"?.*/\1/' || echo "0")
    tn=$(grep -E '"true_negative_detections":' test/results.json 2>/dev/null | head -n1 | sed -E 's/.*"true_negative_detections":[[:space:]]*"?([^",]+)"?.*/\1/' || echo "0")
    errs=$(grep -E '"http_errors":' test/results.json 2>/dev/null | head -n1 | sed -E 's/.*"http_errors":[[:space:]]*"?([^",]+)"?.*/\1/' || echo "0")
    
    echo "  • Latência P99:       ${p99}"
    echo "  • Taxa de Falha:      ${fail_rate}"
    echo "  • Matriz de Confusão: TP=$tp | TN=$tn | FP=$fp | FN=$fn | HTTP Errs=$errs"
    echo "  • 🏆 PONTUAÇÃO FINAL: ${final_score}"
  else
    echo "ℹ️  Arquivo test/results.json não gerado (teste rápido/smoke test sem scoring ponderado)."
  fi
  echo "=========================================================="
} | tee "$STATS_FILE" > "$REPORT_FILE"

cat "$REPORT_FILE"

# 8. Teardown
echo ""
echo "🧹 Desligando containers..."
docker compose $COMPOSE_ARGS down
echo "✅ Concluído! Relatório salvo em $REPORT_FILE e $STATS_FILE."
