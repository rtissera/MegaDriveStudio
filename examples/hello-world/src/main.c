/**
 * Megadrive Studio — Template projet
 *
 * Point de départ pour un homebrew SGDK 2.x
 * Debug : F5 dans VS Code (config BlastEm GDB pipe)
 * KDebug : kprintf() → affiché dans l'émulateur + terminal VS Code
 */

#include <genesis.h>

// ── KDebug helpers ────────────────────────────────────────────────────────────
// Sur émulateur (BlastEm, ClownMDEmu) : affiche dans la console debug
// Sur hardware (ED Pro) : envoie via USB après init SSF mapper
// Sur hardware sans init : no-op (harmless)
#ifdef DEBUG
  #define DBG(fmt, ...) KDebug_Alert(fmt, ##__VA_ARGS__)
#else
  #define DBG(fmt, ...) ((void)0)
#endif

// ── Prototypes ────────────────────────────────────────────────────────────────
static void game_init(void);
static void game_loop(void);
static void handle_input(u16 joy, u16 changed, u16 state);

// ── Variables globales ────────────────────────────────────────────────────────
static u16 frame_count = 0;

// ═════════════════════════════════════════════════════════════════════════════
int main(bool hardReset)
{
    game_init();

    DBG("Megadrive Studio — init OK");
    DBG("SGDK version : %d", SGDK_VERSION);

    game_loop();

    // Ne devrait jamais arriver
    return 0;
}

// ─────────────────────────────────────────────────────────────────────────────
static void game_init(void)
{
    // Init hardware
    VDP_setScreenWidth320();

    // Palette de base : blanc sur fond noir
    PAL_setColor(0, RGB24_TO_VDPCOLOR(0x000000));
    PAL_setColor(1, RGB24_TO_VDPCOLOR(0xFFFFFF));

    // Texte de bienvenue
    VDP_setTextPlane(BG_A);
    VDP_setTextPalette(PAL0);
    VDP_drawText("MEGADRIVE STUDIO", 2, 2);
    VDP_drawText("F5 in VS Code to debug", 2, 4);

    // Joystick
    JOY_init();
    JOY_setEventHandler(handle_input);
}

// ─────────────────────────────────────────────────────────────────────────────
static void game_loop(void)
{
    while (TRUE)
    {
        // Logique de jeu ici

        // Debug : log toutes les 60 frames
        if ((frame_count % 60) == 0)
        {
            DBG("Frame %d", frame_count);
        }

        frame_count++;
        SYS_doVBlankProcess();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
static void handle_input(u16 joy, u16 changed, u16 state)
{
    if (joy != JOY_1) return;

    if (changed & state & BUTTON_START)
    {
        DBG("START pressed, frame=%d", frame_count);
    }

    if (changed & state & BUTTON_A)
    {
        DBG("BUTTON_A pressed");
    }
}
