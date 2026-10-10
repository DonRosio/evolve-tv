#!/usr/bin/env bash
# Mesure la mémoire réellement utilisée par Evolve TV et ses processus enfants
# (streamlink, mpv).
#
# - « RAM » : PSS (la mémoire partagée est répartie équitablement entre les processus).
# - « swap » : part déplacée dans le swap (zram compressé sur ce PC) quand la RAM manque.
#   Sans cette colonne, la mesure paraît plus basse qu'elle ne l'est.
set -euo pipefail

app=$(pgrep -n -x evolve-tv) || { echo "Evolve TV n'est pas lancé."; exit 1; }
total_ram=0
total_swap=0
printf "%-12s %8s %8s %8s\n" "" "RAM" "swap" "total"
for pid in "$app" $(pgrep -P "$app" || true); do
    name=$(ps -o comm= -p "$pid") || continue
    read -r ram swap < <(awk '/^Pss:/ {p=$2} /^SwapPss:/ {s=$2} END {print p+0, s+0}' \
        "/proc/$pid/smaps_rollup" 2>/dev/null) || continue
    printf "%-12s %5d Mo %5d Mo %5d Mo\n" "$name" $((ram / 1024)) $((swap / 1024)) $(((ram + swap) / 1024))
    total_ram=$((total_ram + ram))
    total_swap=$((total_swap + swap))
done
printf "%-12s %5d Mo %5d Mo %5d Mo\n" "TOTAL" $((total_ram / 1024)) $((total_swap / 1024)) \
    $(((total_ram + total_swap) / 1024))
