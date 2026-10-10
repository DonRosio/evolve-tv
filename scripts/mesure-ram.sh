#!/usr/bin/env bash
# Mesure la RAM réellement utilisée (PSS) par Evolve TV et ses processus enfants
# (streamlink, mpv). Le PSS répartit équitablement la mémoire partagée entre les
# processus : c'est la mesure la plus juste pour comparer des applications.
set -euo pipefail

app=$(pgrep -n -x evolve-tv) || { echo "Evolve TV n'est pas lancé."; exit 1; }
total=0
for pid in "$app" $(pgrep -P "$app" || true); do
    name=$(ps -o comm= -p "$pid") || continue
    kb=$(awk '/^Pss:/ {print $2}' "/proc/$pid/smaps_rollup" 2>/dev/null) || continue
    printf "%-12s %5d Mo\n" "$name" $((kb / 1024))
    total=$((total + kb))
done
printf "%-12s %5d Mo\n" "TOTAL" $((total / 1024))
