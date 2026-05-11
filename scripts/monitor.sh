#!/bin/sh
while true; do
  docker stats --no-stream --format "container={{.Name}},cpu={{.CPUPerc}},mem={{.MemUsage}},mem_perc={{.MemPerc}}" 2026-lb-1 2026-api1-1 2026-api2-1 >> resource_usage.log
  sleep 5
done
