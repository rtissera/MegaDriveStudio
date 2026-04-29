# SKILL: megadrive — Hardware Mega Drive / Sega Genesis

## Carte mémoire 68k
```
$000000-$3FFFFF   ROM cartouche (4MB, SSF2 mapper pour +)
$400000-$7FFFFF   zone non allouée / RAM externe (32X, Mega CD...)
$A00000-$A0FFFF   Z80 address space (vu depuis 68k)
  $A00000-$A01FFF   Z80 RAM (8KB)
  $A04000-$A04001   YM2612 port 0
  $A04002-$A04003   YM2612 port 1
$A10000-$A1001F   I/O (joysticks, région, etc.)
  $A10001           version hardware (bits: 0=overseas, 6=PAL)
  $A10003           data port joypad 1
  $A10005           data port joypad 2
  $A10009           ctrl port joypad 1 (direction IN/OUT)
$A11000           Z80 BUSREQ (write $100 = demande bus, $000 = libère)
$A11100           Z80 BUSREQ status (read bit 8 = busy)
$A11200           Z80 RESET ($000 = reset, $100 = run)
$A130xx           SSF2 mapper / ED Pro USB
  $A130E2           USB data register (Mega ED Pro, SSF mapper actif)
  $A130F1           SRAM access control
$C00000           VDP data port (r/w)
$C00002           VDP data port miroir
$C00004           VDP control port (r/w) — write = commande/address
$C00006           VDP control port miroir
$C00008           H/V counter (read only)
$C0001C           debug register (KDebug)
$FF0000-$FFFFFF   68k RAM (64KB)
```

## VDP — registres de contrôle
Commande d'écriture : `move.l #$SSSSAAAA, VDP_CTRL`  
où SS = type (00=VRAM r, 01=VRAM w, 10=CRAM w, 11=VSRAM w) + bits de l'adresse

```
Reg  0  : mode set 1  (HInt enable b4, HV counter stop b1)
Reg  1  : mode set 2  (display enable b6, VInt enable b5, DMA enable b4, PAL b3)
Reg  2  : plan A name table address (bits 12-15 de l'addr VRAM, /= 0x2000)
Reg  3  : window name table address
Reg  4  : plan B name table address
Reg  5  : sprite attribute table address
Reg  7  : background color (palette × 16 + index)
Reg 10  : HInt counter (ligne toutes les n+1 lignes)
Reg 11  : mode set 3  (external int b3, VScroll mode b2, HScroll mode b0-1)
Reg 12  : mode set 4  (RS0+RS1 = H40/H32, shadow/highlight b3)
Reg 13  : HScroll data address (bits 9-15 de l'addr VRAM)
Reg 15  : auto-increment après chaque accès VDP
Reg 16  : plane size (HxV : 0=32, 1=64, 3=128 tiles)
Reg 17  : window X position
Reg 18  : window Y position
Reg 19-20: DMA length (low/high word count)
Reg 21-23: DMA source address (low/mid/high, décalé de 1 bit)
```

## Tile / Tilemap format
```
Tile data VRAM : 32 bytes par tile 8×8, 4bpp (2 pixels par byte)
  octet N = (pixel 2N couleur 4 bits) | (pixel 2N+1 couleur 4 bits)
  ligne 0 en premier, de gauche à droite

Entrée tilemap (16 bits) :
  bit 15    : priorité (1 = devant les sprites priorité basse)
  bit 14-13 : palette (0-3)
  bit 12    : flip vertical
  bit 11    : flip horizontal
  bit 10-0  : index tile (0-2047)
```

## Sprites hardware
```
Table sprites VRAM (512 bytes max = 80 sprites en H40)
Entrée sprite (8 bytes) :
  word 0  : Y position (0-511, offset +128)
  word 1  : size (bits 8-11 = Hn, bits 0-3 = Vn ; n = nb tiles - 1)
            + link (bits 0-6) = index prochain sprite (0 = fin)
  word 2  : attr = priority(1) + palette(2) + flipV(1) + flipH(1) + tile_index(11)
  word 3  : X position (0-511, offset +128)
```

## Timing frame
```
NTSC 60Hz :  262 lignes × ~341 pixels = ~89342 master clocks/frame
  Lignes 0-223  : active display (224 lignes visibles)
  Lignes 224-261: VBlank (~2.4ms)
  Chaque ligne  : HBlank (~20µs)

PAL 50Hz : 313 lignes
  Lignes 0-239 ou 0-223 selon mode (240 ou 224 visibles)

VDP FIFO : 4 words — si plein, 68k stoppé (wait states)
DMA max speed : 1 word/2 master clocks en HBlank, 1 word/8 en active
```

## YM2612 (OPN2 — 6 canaux FM)
```
Registres accessibles depuis Z80 ($4000/$4001 = port0, $4002/$4003 = port1)
Depuis 68k : $A04000-$A04003 (via Z80 bus request)

Canal 1-3 → port 0 (index $00-$9F)
Canal 4-6 → port 1 (index $00-$9F)

Key On/Off : reg $28, valeur 0xF0-0xF5 (on) / 0x00-0x05 (off)
DAC mode   : reg $2B bit7 = 1 (désactive canal 6 FM, active DAC 8-bit)
DAC data   : reg $2A (écrire sample PCM 8-bit, 53267 Hz max)
```

## SN76489 PSG (4 canaux : 3 ton + 1 bruit)
```
Port : $7F11 (Z80) / $C00011 (68k via VDP... non, via Z80 window)
Format octet : 1CCCTXXX (latch) ou 0-XXXXXX (data)
CCC = canal (0-3), T = type (0=freq, 1=volume), XXX = bits données
Volume : $F = silence, $0 = max
```

## Flags dans le ROM header ($000100-$0001FF)
```
$100 : "SEGA MEGA DRIVE" ou "SEGA GENESIS"
$110 : Copyright / release date
$120 : Domestic name (48 chars)
$150 : International name (48 chars)
$180 : Version string
$18E : Checksum (u16, ignoré par la plupart des émulateurs)
$190 : I/O support ("J" = joypad 3btn, "6" = 6btn, "M" = mouse...)
$1A0 : ROM start address (usually $00000000)
$1A4 : ROM end address
$1A8 : RAM start ($00FF0000)
$1AC : RAM end   ($00FFFFFF)
$1B0 : SRAM ("RA" + flags si SRAM présente)
$1BC : Modem support (laisser vide)
$1C8 : Notes (9 bytes)
$1D1 : Countries ("JUE" = Japan/US/Europe)
```

## SSF2 mapper (nécessaire pour ROM > 512KB OU USB ED Pro)
```
Registres $A130F3-$A130FF : 8 slots × 512KB
  $A130F3 : slot 1 (addr $080000-$0FFFFF)  défaut bank 1
  $A130F5 : slot 2 (addr $100000-$17FFFF)  défaut bank 2
  ...
  $A130FF : slot 7 (addr $380000-$3FFFFF)  défaut bank 7
  Slot 0 ($000000-$07FFFF) = toujours bank 0 (non remappable)

Header doit contenir "SEGA SSF" à $100 pour activer le mapper.
SGDK : #define ENABLE_BANK_SWITCH 1 dans config.h + rebuild lib
```
