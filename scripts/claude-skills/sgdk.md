# SKILL: sgdk — SGDK 2.x API cheatsheet

## Build
```bash
# Debug (avec symboles GDB)
make -f $GDK/makefile.gen EXTRA_CFLAGS="-g -gdwarf-4 -O0" EXTRA_DEF="-DDEBUG"
# Release
make -f $GDK/makefile.gen EXTRA_CFLAGS="-O2"
# Rebuild lib
make -f $GDK/makelib.gen
```

## Ressources (rescomp — fichier .res)
```
PALETTE  mypal    "gfx/sprite.png"
IMAGE    myimg    "gfx/bg.png"        NONE       ; tileset non compressé
SPRITE   mysprite "gfx/spr.png"      4 4 NONE 0 ; 4x4 tiles, pas de compression
SOUND    mysound  "sfx/shoot.wav"     XGM2       ; son PCM pour XGM2
MUSIC    mybgm    "music/theme.vgm"   XGM2       ; VGM → XGM2
```

## VDP (Video Display Processor)

### Init / config
```c
VDP_setScreenWidth320();    // H40 mode (320x224) — standard
VDP_setScreenWidth256();    // H32 mode (256x224) — certains jeux PAL
VDP_setPlaneSize(64, 32);   // taille plane en tiles (défaut: 64x32)
VDP_setScrollingMode(HSCROLL_PLANE, VSCROLL_PLANE);
```

### Backgrounds (BG_A, BG_B, WINDOW)
```c
// Upload tileset
VDP_loadTileSet(myimg.tileset, TILE_USER_INDEX, DMA);

// Dessiner une tile sur un plan
VDP_setTileMapXY(BG_A, TILE_ATTR_FULL(PAL0, 0, FALSE, FALSE, TILE_USER_INDEX+n),
                 x_tile, y_tile);

// Scroll
VDP_setHorizontalScroll(BG_A, -camera_x);
VDP_setVerticalScroll(BG_A, camera_y);

// Texte debug (plan TEXT par défaut = BG_A ou BG_B)
VDP_drawText("hello", col, row);
VDP_clearText(col, row, len);
```

### Sprites hardware
```c
// Via système objet SGDK
Sprite* spr = SPR_addSprite(&mysprite_def, x, y, TILE_ATTR(PAL1, 0, FALSE, FALSE));
SPR_setPosition(spr, x, y);
SPR_setAnim(spr, anim_index);
SPR_setFrame(spr, frame);
SPR_update();   // à appeler 1x par frame AVANT SYS_doVBlankProcess()
```

### Palettes
```c
PAL_setPalette(PAL0, mypal.data, DMA);
PAL_setColor(index, RGB24_TO_VDPCOLOR(0xRRGGBB));
// Palette MD : 9-bit (3-bit par canal), couleurs valides = 0x000, 0x200, 0x400...0xE00...
```

### DMA
```c
DMA_doDma(DMA_VRAM, (u32)data, vram_addr, len_words, 2);  // transfer vers VRAM
DMA_waitCompletion();
// Attention : DMA_VRAM stoppé si Z80 sur le bus — gérer le bus Z80 avant
```

## Audio XGM2
```c
XGM2_init();
XGM2_playMusic(mybgm);
XGM2_stopMusic();
XGM2_playPCMEx(mysound, SOUND_PCM_CH1, 0);
XGM2_setMusicTempo(150);   // BPM
```

## Input
```c
JOY_init();
JOY_setEventHandler(my_joy_handler);
// handler : (u16 joy, u16 changed, u16 state)
// joy = JOY_1 / JOY_2
// state & BUTTON_A/B/C/X/Y/Z/START/UP/DOWN/LEFT/RIGHT

// Polling direct (si pas d'event handler)
u16 buttons = JOY_readJoypad(JOY_1);
if (buttons & BUTTON_RIGHT) player_x++;
```

## Système / timing
```c
SYS_doVBlankProcess();  // FIN de frame — attend VBlank, flush DMA, update sprites
SYS_setVIntCallback(my_vint_handler);   // VBlank interrupt handler
SYS_setHIntCallback(my_hint_handler);   // HBlank interrupt handler
VDP_setHIntCounter(n);                  // HInt toutes les n+1 lignes

u32 tick = SYS_getFrameCount();         // compteur frames (u32)
```

## Mémoire
```c
// ROM  : $000000-$3FFFFF (4MB max, SSF2 mapper pour plus)
// RAM  : $FF0000-$FFFFFF (64KB) — variables C ici
// VRAM : 64KB — tiles, tables de plans, table sprites
// CRAM : 128 bytes — 4 palettes × 16 couleurs × 2 bytes
// VSRAM: 80 bytes — scroll vertical (40 entrées)
// SRAM : $200001-$20FFFF (si activé dans header)

MEM_alloc(size);    // allocateur SGDK (malloc-like, heap dans RAM)
MEM_free(ptr);
```

## KDebug (debug printf)
```c
KDebug_Alert("val=%d", my_var);   // → terminal BlastEm/ClownMDEmu
// Équivalent SGDK : kprintf (wrapper KDebug_Alert)
// Uniquement actif si la ROM est lancée dans un émulateur supportant KDebug
// Sur vrai hardware : no-op (écrit dans un registre VDP inutilisé)
```

## Pièges fréquents
- Appeler `SPR_update()` AVANT `SYS_doVBlankProcess()` — dans l'autre ordre, les sprites ne se mettent pas à jour
- TILE_USER_INDEX commence à la tile 256 (tiles 0-255 = font SGDK)
- `RGB24_TO_VDPCOLOR` arrondit à la précision 9-bit MD — les couleurs PC ne sont pas exactes
- DMA depuis ROM : adresse source doit être paire et la ROM accessible (pas de SSF2 bank non sélectionnée)
- VDP_drawText sur un plan scrollé → utiliser WINDOW plane pour les HUD fixes
- `SYS_doVBlankProcess()` bloque jusqu'au prochain VBlank — budget CPU ≈ 312 lignes × ~70 cycles/ligne ≈ 65K cycles à 7.67 MHz ≈ ~8.5ms
