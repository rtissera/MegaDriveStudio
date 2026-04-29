#!/usr/bin/env python3
"""
scripts/claude-build-assist.py
================================
Intégration Claude API pour assistance debug Mega Drive.

Modes :
  --mode build   : analyse les erreurs de compilation (stdin = output make)
  --mode klog    : analyse un fichier de log KDebug
  --mode asm     : analyse un fichier ASM 68k

Usage :
  make debug 2>&1 | python3 scripts/claude-build-assist.py
  python3 scripts/claude-build-assist.py --mode klog --file /tmp/kdebug.log
  python3 scripts/claude-build-assist.py --mode asm --file out/rom.asm
"""

import argparse
import json
import os
import sys
import urllib.request
import urllib.error
from pathlib import Path

API_URL = "https://api.anthropic.com/v1/messages"
MODEL   = "claude-haiku-4-5-20251001"   # Haiku : rapide et cheap pour cette tâche

SYSTEM_PROMPT = """Tu es un expert en développement homebrew Sega Mega Drive / Genesis.
Tu connais parfaitement :
- SGDK 2.x (C, ASM 68k, ressources rescomp)
- Le toolchain m68k-elf-gcc / m68k-elf-gdb
- L'architecture hardware MD (68000, Z80, VDP, YM2612, PSG)
- Les erreurs communes de compilation SGDK
- BlastEm et ClownMDEmu (debug emulateurs)
- Le Mega Everdrive Pro (protocole USB, SSF mapper)

Réponds en français. Sois concis et direct. Donne des suggestions de fix concrètes avec du code quand pertinent.
"""

def read_source_files(workspace: str, max_files: int = 3) -> str:
    """Lit les fichiers source C du projet (pour contexte)."""
    src_dir = Path(workspace) / "src"
    sources = []
    for f in sorted(src_dir.glob("*.c"))[:max_files]:
        try:
            content = f.read_text(errors='replace')
            # Tronque si trop long
            if len(content) > 2000:
                content = content[:2000] + "\n... [tronqué]"
            sources.append(f"// === {f.name} ===\n{content}")
        except Exception:
            pass
    return "\n\n".join(sources) if sources else "(pas de sources trouvées)"


def call_claude(system: str, user_message: str) -> str:
    """Appelle l'API Claude."""
    api_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not api_key:
        return "[ERREUR] ANTHROPIC_API_KEY non définie. Export la variable et relance."

    payload = json.dumps({
        "model": MODEL,
        "max_tokens": 1024,
        "system": system,
        "messages": [{"role": "user", "content": user_message}]
    }).encode()

    req = urllib.request.Request(
        API_URL,
        data=payload,
        headers={
            "Content-Type": "application/json",
            "x-api-key": api_key,
            "anthropic-version": "2023-06-01"
        },
        method="POST"
    )

    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read())
            return data["content"][0]["text"]
    except urllib.error.HTTPError as e:
        body = e.read().decode()
        return f"[ERREUR API {e.code}] {body[:200]}"
    except Exception as e:
        return f"[ERREUR] {e}"


def mode_build(args):
    """Analyse les erreurs de compilation depuis stdin."""
    build_output = sys.stdin.read()
    if not build_output.strip():
        print("[claude] Aucun output de build reçu sur stdin")
        return

    workspace = os.getcwd()
    sources = read_source_files(workspace)

    # Filtre pour ne garder que les erreurs/warnings
    error_lines = [l for l in build_output.splitlines()
                   if any(k in l for k in ["error:", "warning:", "undefined", "fatal:"])]
    error_summary = "\n".join(error_lines[:30]) if error_lines else build_output[:1500]

    user_msg = f"""Analyse ces erreurs de compilation SGDK/m68k-elf-gcc et propose des fixes :

=== ERREURS ===
{error_summary}

=== CODE SOURCE ===
{sources}

Quels sont les problèmes et comment les corriger ?"""

    print("\n\033[1m\033[36m── Claude — analyse build ──────────────────────\033[0m")
    print(call_claude(SYSTEM_PROMPT, user_msg))
    print("\033[36m────────────────────────────────────────────────\033[0m\n")


def mode_klog(args):
    """Analyse un fichier de log KDebug."""
    if not args.file:
        print("[claude] --file requis pour le mode klog")
        return
    try:
        log_content = Path(args.file).read_text(errors='replace')
    except FileNotFoundError:
        print(f"[claude] Fichier non trouvé : {args.file}")
        return

    # Garde les 100 dernières lignes
    lines = log_content.splitlines()
    recent = "\n".join(lines[-100:]) if len(lines) > 100 else log_content

    workspace = os.getcwd()
    sources = read_source_files(workspace)

    user_msg = f"""Analyse ce log KDebug d'un homebrew Mega Drive et identifie les problèmes potentiels :

=== LOG KDEBUG (dernières lignes) ===
{recent}

=== CODE SOURCE ===
{sources}

Y a-t-il des patterns suspects, des erreurs ou des comportements anormaux ?"""

    print("\n\033[1m\033[36m── Claude — analyse KDebug ─────────────────────\033[0m")
    print(call_claude(SYSTEM_PROMPT, user_msg))
    print("\033[36m────────────────────────────────────────────────\033[0m\n")


def mode_asm(args):
    """Analyse un fichier ASM 68k."""
    if not args.file:
        print("[claude] --file requis pour le mode asm")
        return
    try:
        asm_content = Path(args.file).read_text(errors='replace')
    except FileNotFoundError:
        print(f"[claude] Fichier non trouvé : {args.file}")
        return

    # Tronque
    if len(asm_content) > 4000:
        asm_content = asm_content[:4000] + "\n... [tronqué]"

    user_msg = f"""Analyse cet assembleur 68k généré par m68k-elf-gcc pour un homebrew Mega Drive.
Identifie les inefficacités éventuelles ou problèmes notables :

=== ASM ===
{asm_content}"""

    print("\n\033[1m\033[36m── Claude — analyse ASM ────────────────────────\033[0m")
    print(call_claude(SYSTEM_PROMPT, user_msg))
    print("\033[36m────────────────────────────────────────────────\033[0m\n")


def main():
    p = argparse.ArgumentParser(description="Claude API — assistant debug Mega Drive")
    p.add_argument("--mode", choices=["build", "klog", "asm"], default="build")
    p.add_argument("--file", default=None, help="Fichier à analyser (klog, asm)")
    args = p.parse_args()

    if args.mode == "build":
        mode_build(args)
    elif args.mode == "klog":
        mode_klog(args)
    elif args.mode == "asm":
        mode_asm(args)


if __name__ == "__main__":
    main()
