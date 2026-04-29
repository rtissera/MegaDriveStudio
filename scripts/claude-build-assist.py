#!/usr/bin/env python3
"""
scripts/claude-build-assist.py
================================
Intégration Claude API — assistant debug Mega Drive.
Supporte un système de skills (contexte expert injecté dans le prompt)
et un mode "caveman" (réduction drastique des tokens).

SKILLS disponibles (scripts/claude-skills/) :
  m68k       — instruction set M68000, registres, cycles, patterns
  z80        — Z80 sur MD, bus arbitration, drivers son
  sgdk       — API SGDK 2.x, rescomp, build flags
  megadrive  — hardware MD, carte mémoire, VDP, YM2612, timing

MODES :
  build      stdin = sortie make, analyse erreurs de compilation
  klog       --file = log KDebug, détecte patterns anormaux
  asm        --file = output ASM 68k, analyse inefficacités

OPTIONS :
  --caveman       mode ultra-compact : ≤5 lignes, pas de markdown, 256 tokens max
  --skills LIST   forcer skills (ex: --skills m68k,sgdk)
  --no-skills     prompt minimal sans injection de skills
  --max-tokens N  override max_tokens
  --mode skills-info  afficher skills disponibles

USAGE :
  make debug 2>&1 | python3 scripts/claude-build-assist.py
  make debug 2>&1 | python3 scripts/claude-build-assist.py --caveman
  python3 scripts/claude-build-assist.py --mode klog --file /tmp/kdebug.log
  python3 scripts/claude-build-assist.py --mode asm --file out/rom.asm --caveman --skills m68k
"""

import argparse
import json
import os
import re
import sys
import urllib.request
import urllib.error
from pathlib import Path

# ── Config ────────────────────────────────────────────────────────────────────
API_URL            = "https://api.anthropic.com/v1/messages"
MODEL              = "claude-haiku-4-5-20251001"
SKILLS_DIR         = Path(__file__).parent / "claude-skills"
MAX_TOKENS_NORMAL  = 1024
MAX_TOKENS_CAVEMAN = 256
SKILLS_BUDGET      = 6000   # chars max de skills injectés dans le prompt

# ── Détection automatique des skills selon le contenu ─────────────────────────
SKILL_TRIGGERS = {
    "m68k": [
        r"\.s:", r"\.68k:", r"\basm\b", r"\bregister\b", r"move\.[bwl]",
        r"\blea\b", r"\bjsr\b", r"\bD[0-7]\b", r"\bA[0-6]\b",
        r"illegal instruction", r"bus error", r"address error",
        r"\bexception\b", r"\binterrupt\b", r"\bvector\b",
        r"m68k-elf-", r"\.68k"
    ],
    "z80": [
        r"\bz80\b", r"\bZ80\b", r"YM2612", r"ym2612", r"SN76489",
        r"\bpsg\b", r"\bPSG\b", r"\bXGM\b", r"\bGEMS\b",
        r"sound.?driver", r"\baudio\b", r"busreq", r"BUSREQ",
        r"A00000", r"A04000", r"7F11"
    ],
    "sgdk": [
        r"genesis\.h", r"\bVDP_", r"\bSPR_", r"\bPAL_", r"\bJOY_",
        r"\bDMA_", r"\bXGM2_", r"\bSYS_", r"\bMEM_",
        r"\bBG_[AB]\b", r"\bTILE_", r"makefile\.gen", r"\brescomp\b",
        r"\.res:", r"\blibmd\b", r"\bSGDK\b", r"\bGDK\b"
    ],
    "megadrive": [
        r"C0000[0-9]", r"FF0000", r"A130[0-9A-Fa-f]", r"A10[0-9A-Fa-f]{3}",
        r"\bVRAM\b", r"\bCRAM\b", r"\bVSRAM\b",
        r"mega.?drive", r"genesis", r"Mega Drive"
    ]
}


def detect_skills(text: str) -> list:
    skills = []
    for skill, patterns in SKILL_TRIGGERS.items():
        for pat in patterns:
            if re.search(pat, text, re.IGNORECASE):
                skills.append(skill)
                break
    # sgdk et megadrive toujours utiles pour ce projet
    for always in ("sgdk", "megadrive"):
        if always not in skills:
            skills.append(always)
    return skills


def load_skill(name: str) -> str:
    f = SKILLS_DIR / f"{name}.md"
    return f.read_text(encoding="utf-8", errors="replace") if f.exists() else ""


def build_skills_context(names: list, budget: int = SKILLS_BUDGET) -> str:
    parts, remaining = [], budget
    for name in names:
        content = load_skill(name)
        if not content:
            continue
        header = f"## [SKILL:{name}]\n"
        if len(header) + len(content) > remaining:
            trunc = content[:remaining - len(header)].rsplit("\n", 1)[0]
            parts.append(header + trunc + "\n...[tronqué]")
            break
        parts.append(header + content)
        remaining -= len(header) + len(content)
    return "\n\n".join(parts)


def make_system_prompt(caveman: bool, skills_ctx: str) -> str:
    base = (
        "Tu es un expert en développement homebrew Sega Mega Drive / Genesis. "
        "Tu connais SGDK 2.x, m68k-elf-gcc/gdb, l'architecture hardware MD "
        "(68000, Z80, VDP, YM2612), BlastEm, ClownMDEmu, Mega Everdrive Pro."
    )
    if caveman:
        style = (
            "\n\nMODE CAVEMAN - REGLES STRICTES :"
            "\n- Reponse 5 lignes max. Zero blabla."
            "\n- Pas de markdown (pas de #, **, backticks)."
            "\n- Cause + fix en clair. Ex: 'line42 manque point-virgule apres move.l'"
            "\n- Si code: une ligne max, pas d'explication."
            "\n- Pas de politesse ni conclusion."
        )
    else:
        style = (
            "\n\nRéponds en français. Concis et direct. "
            "Suggestions de fix avec code minimal si pertinent."
        )
    ctx = f"\n\n## RÉFÉRENCE TECHNIQUE\n{skills_ctx}" if skills_ctx else ""
    return base + style + ctx


# ── API ───────────────────────────────────────────────────────────────────────
def call_claude(system: str, user_msg: str, max_tokens: int) -> str:
    key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not key:
        return "ERREUR: variable ANTHROPIC_API_KEY non definie."

    payload = json.dumps({
        "model": MODEL,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{"role": "user", "content": user_msg}]
    }).encode()

    req = urllib.request.Request(
        API_URL, data=payload,
        headers={
            "Content-Type":    "application/json",
            "x-api-key":       key,
            "anthropic-version": "2023-06-01"
        },
        method="POST"
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read())["content"][0]["text"]
    except urllib.error.HTTPError as e:
        return f"API {e.code}: {e.read().decode()[:200]}"
    except Exception as e:
        return f"ERREUR: {e}"


def read_sources(max_chars: int = 1500) -> str:
    src = Path(os.getcwd()) / "src"
    parts = []
    for f in sorted(src.glob("*.c"))[:3]:
        try:
            c = f.read_text(errors="replace")
            parts.append(f"// {f.name}\n{c[:max_chars]}" + ("\n...[tronqué]" if len(c) > max_chars else ""))
        except Exception:
            pass
    return "\n\n".join(parts) or "(pas de sources)"


def print_response(mode: str, text: str, caveman: bool):
    if caveman:
        print(f"\n[claude/{mode}] {text.strip()}\n")
    else:
        bar = "─" * 50
        print(f"\n\033[1;36m── Claude [{mode}] {bar}\033[0m")
        print(text)
        print(f"\033[36m{'─' * 60}\033[0m\n")


# ── Modes ─────────────────────────────────────────────────────────────────────
def mode_build(args, skill_names, max_tokens):
    raw = sys.stdin.read()
    if not raw.strip():
        print("[claude] stdin vide — pipe la sortie de make")
        return

    if not args.no_skills and args.skills is None:
        skill_names = detect_skills(raw)
        if not args.caveman:
            print(f"\033[90m[claude] skills: {', '.join(skill_names)}\033[0m")

    errors = [l for l in raw.splitlines()
              if any(k in l for k in ["error:", "warning:", "undefined", "fatal:", "Error"])]
    summary = "\n".join(errors[:40]) if errors else raw[:2000]

    ctx = build_skills_context(skill_names, 3000 if args.caveman else SKILLS_BUDGET)
    system = make_system_prompt(args.caveman, ctx)

    msg = f"ERREURS COMPILATION :\n{summary}"
    if not args.caveman:
        msg += f"\n\nSOURCES :\n{read_sources()}"
    msg += "\n\nFix ?"

    print_response("build", call_claude(system, msg, max_tokens), args.caveman)


def mode_klog(args, skill_names, max_tokens):
    if not args.file:
        print("[claude] --file requis"); return
    try:
        content = Path(args.file).read_text(errors="replace")
    except FileNotFoundError:
        print(f"[claude] fichier non trouvé: {args.file}"); return

    lines = content.splitlines()
    recent = "\n".join(lines[-80:]) if len(lines) > 80 else content

    if not args.no_skills and args.skills is None:
        skill_names = detect_skills(content)

    ctx = build_skills_context(skill_names, 3000 if args.caveman else SKILLS_BUDGET)
    system = make_system_prompt(args.caveman, ctx)

    msg = f"LOG KDEBUG :\n{recent}"
    if not args.caveman:
        msg += f"\n\nSOURCES :\n{read_sources(1200)}"
    msg += "\n\nProblèmes ?"

    print_response("klog", call_claude(system, msg, max_tokens), args.caveman)


def mode_asm(args, skill_names, max_tokens):
    if not args.file:
        print("[claude] --file requis"); return
    try:
        content = Path(args.file).read_text(errors="replace")
    except FileNotFoundError:
        print(f"[claude] fichier non trouvé: {args.file}"); return

    if len(content) > 4000:
        content = content[:4000] + "\n...[tronqué]"

    if not args.no_skills and args.skills is None:
        skill_names = ["m68k", "sgdk"]

    ctx = build_skills_context(skill_names, SKILLS_BUDGET)
    system = make_system_prompt(args.caveman, ctx)

    msg = f"ASM 68k (m68k-elf-gcc output) :\n{content}\n\nProblèmes ou inefficacités ?"
    print_response("asm", call_claude(system, msg, max_tokens), args.caveman)


def cmd_skills_info():
    print("\n[claude-skills] Skills disponibles :\n")
    for f in sorted(SKILLS_DIR.glob("*.md")):
        content = f.read_text(errors="replace")
        lines = content.count("\n")
        print(f"  {f.stem:<14} {len(content):>5} chars  {lines:>4} lignes  →  {f}")
    print()


# ── Main ──────────────────────────────────────────────────────────────────────
def main():
    p = argparse.ArgumentParser(
        description="Claude API — assistant debug Mega Drive avec skills + caveman mode",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__
    )
    p.add_argument("--mode",
                   choices=["build", "klog", "asm", "skills-info"],
                   default="build")
    p.add_argument("--file", default=None)
    p.add_argument("--skills", default=None,
                   help="skills forcés séparés par virgule (m68k,sgdk,...)")
    p.add_argument("--no-skills",  action="store_true")
    p.add_argument("--caveman",    action="store_true",
                   help="mode ultra-compact : ≤5 lignes, pas de markdown")
    p.add_argument("--max-tokens", type=int, default=None)
    args = p.parse_args()

    if args.mode == "skills-info":
        cmd_skills_info(); return

    skill_names = (
        []
        if args.no_skills else
        [s.strip() for s in args.skills.split(",") if s.strip()]
        if args.skills else
        []   # sera auto-détecté dans chaque mode
    )
    max_tokens = (
        args.max_tokens if args.max_tokens else
        MAX_TOKENS_CAVEMAN if args.caveman else
        MAX_TOKENS_NORMAL
    )

    if   args.mode == "build": mode_build(args, skill_names, max_tokens)
    elif args.mode == "klog":  mode_klog(args, skill_names, max_tokens)
    elif args.mode == "asm":   mode_asm(args, skill_names, max_tokens)


if __name__ == "__main__":
    main()
