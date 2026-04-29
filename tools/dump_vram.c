// SPDX-License-Identifier: MIT
/*
 * M0 spike: load ClownMDEmu via libra, run a ROM headless for 600 frames,
 * dump VRAM to disk.
 *
 * Usage: dump_vram <rom> [vram_out_path]
 * Defaults: vram_out_path = vram.bin
 */
#include "libra.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#define RETRO_MEMORY_VIDEO_RAM 3
#define DEFAULT_FRAMES         600
#define DEFAULT_CORE_PATH \
    "vendor/clownmdemu-libretro/clownmdemu_libretro.so"

/* No-op callbacks: headless run, no display, no audio, no input. */
static void    video_cb(void *ud, const void *d, unsigned w, unsigned h,
                        size_t p, int f) {
    (void)ud; (void)d; (void)w; (void)h; (void)p; (void)f;
}
static void    audio_cb(void *ud, const int16_t *d, size_t n) {
    (void)ud; (void)d; (void)n;
}
static void    input_poll_cb(void *ud) { (void)ud; }
static int16_t input_state_cb(void *ud, unsigned p, unsigned d, unsigned i, unsigned id) {
    (void)ud; (void)p; (void)d; (void)i; (void)id;
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <rom> [vram_out]\n", argv[0]);
        return 2;
    }
    const char *rom_path  = argv[1];
    const char *vram_path = (argc >= 3) ? argv[2] : "vram.bin";
    const char *core_path = getenv("MDS_CORE_PATH");
    if (!core_path || !*core_path) core_path = DEFAULT_CORE_PATH;

    libra_config_t cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.video             = video_cb;
    cfg.audio             = audio_cb;
    cfg.input_poll        = input_poll_cb;
    cfg.input_state       = input_state_cb;
    cfg.audio_output_rate = 48000;

    libra_ctx_t *ctx = libra_create(&cfg);
    if (!ctx) { fprintf(stderr, "libra_create failed\n"); return 1; }

    libra_set_system_directory(ctx, "/tmp");
    libra_set_save_directory(ctx, "/tmp");
    libra_set_assets_directory(ctx, "/tmp");

    if (!libra_load_core(ctx, core_path)) {
        fprintf(stderr, "libra_load_core failed: %s\n", core_path);
        libra_destroy(ctx);
        return 1;
    }
    if (!libra_load_game(ctx, rom_path)) {
        fprintf(stderr, "libra_load_game failed: %s\n", rom_path);
        libra_unload_core(ctx);
        libra_destroy(ctx);
        return 1;
    }

    for (int i = 0; i < DEFAULT_FRAMES; i++) libra_run(ctx);

    void  *vram = libra_get_memory_data(ctx, RETRO_MEMORY_VIDEO_RAM);
    size_t vsz  = libra_get_memory_size(ctx, RETRO_MEMORY_VIDEO_RAM);

    int rc = 0;
    if (!vram || vsz == 0) {
        fprintf(stderr, "no VIDEO_RAM exposed by core (data=%p size=%zu)\n",
                vram, vsz);
        rc = 3;
    } else {
        FILE *f = fopen(vram_path, "wb");
        if (!f) { perror(vram_path); rc = 4; }
        else {
            size_t n = fwrite(vram, 1, vsz, f);
            fclose(f);
            if (n != vsz) { fprintf(stderr, "short write: %zu/%zu\n", n, vsz); rc = 5; }
        }
    }

    if (rc == 0) {
        const uint8_t *p = (const uint8_t *)vram;
        char hex[33] = {0};
        for (int i = 0; i < 16; i++)
            snprintf(hex + i * 2, 3, "%02x", p[i]);
        printf("OK rom=%s frames=%d vram_size=%zu first16=%s\n",
               rom_path, DEFAULT_FRAMES, vsz, hex);
    }

    libra_unload_game(ctx);
    libra_unload_core(ctx);
    libra_destroy(ctx);
    return rc;
}
