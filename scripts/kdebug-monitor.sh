#!/usr/bin/env bash
# =============================================================================
# scripts/kdebug-monitor.sh
# Écoute les messages KDebug/kprintf envoyés par le ROM via USB ED Pro
#
# Le ROM doit écrire via $A130E2 (SSF mapper USB register) — voir
# l'API edio-mega dans krikzz/mega-ed-pub (edio-mega/src/)
#
# Usage : ./scripts/kdebug-monitor.sh [/dev/everdrive]
# =============================================================================
set -euo pipefail

PORT="${1:-${EDPRO_PORT:-/dev/everdrive}}"
LOGFILE="/tmp/kdebug-$(date +%Y%m%d-%H%M%S).log"

RED='\033[0;31m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; GREEN='\033[0;32m'; NC='\033[0m'

if [[ ! -e "$PORT" ]]; then
  echo -e "${RED}[klog]${NC} Port $PORT non trouvé."
  echo "  Branche le Mega Everdrive Pro et vérifie EDPRO_PORT"
  echo "  Ports disponibles : $(ls /dev/ttyACM* /dev/everdrive 2>/dev/null | tr '\n' ' ')"
  exit 1
fi

echo -e "${GREEN}[klog]${NC} Monitoring KDebug sur $PORT → $LOGFILE"
echo -e "${CYAN}        Ctrl+C pour arrêter${NC}"
echo ""

# Utilise megalink si disponible (parse le protocole ED Pro proprement)
if command -v megalink >/dev/null 2>&1; then
  megalink -p "$PORT" monitor 2>&1 | while IFS= read -r line; do
    TIMESTAMP=$(date +%H:%M:%S.%3N)
    printf "${CYAN}[%s]${NC} %s\n" "$TIMESTAMP" "$line" | tee -a "$LOGFILE"
  done
else
  # Fallback : lecture directe serial (marche si le ROM écrit du texte brut)
  echo -e "${YELLOW}[klog]${NC} megalink non trouvé — lecture serial brute (peut avoir du bruit)"
  stty -F "$PORT" 115200 raw -echo 2>/dev/null || true
  while IFS= read -r -d $'\n' line < "$PORT"; do
    TIMESTAMP=$(date +%H:%M:%S.%3N)
    printf "${CYAN}[%s]${NC} %s\n" "$TIMESTAMP" "$line" | tee -a "$LOGFILE"
  done
fi
