// SPDX-License-Identifier: MIT
//! `mega://*` resource catalogue + read/subscribe routing. The catalogue is
//! the authoritative list returned to `resources/list`. Subscriber delivery
//! happens on the broadcast channel exposed by `EmulatorActor`.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rmcp::model::{AnnotateAble, RawResource, Resource, ResourceContents};

use crate::emulator::{decode, EmulatorActor, MemorySpace};
use crate::notifications::SnapshotCache;

#[derive(Debug, Clone, Copy)]
pub struct ResourceDef {
    pub uri: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime_type: &'static str,
    pub source: ResourceSource,
}

#[derive(Debug, Clone, Copy)]
pub enum ResourceSource {
    Region(MemorySpace),
    /// Decoded VDP register dump.
    VdpRegistersJson,
    /// Decoded sprite attribute table.
    SpritesJson,
    /// Decoded 68k register snapshot.
    M68kJson,
    /// Framebuffer (M3).
    FramebufferPng,
    /// Decoded Z80 register snapshot (M4).
    Z80Json,
    /// Live breakpoint list (M4).
    BreakpointsJson,
}

pub const CATALOGUE: &[ResourceDef] = &[
    ResourceDef {
        uri: "mega://vram",
        name: "VRAM",
        description: "65,536 bytes of VRAM (tiles + nametables + sprite attribute table).",
        mime_type: "application/octet-stream",
        source: ResourceSource::Region(MemorySpace::Vram),
    },
    ResourceDef {
        uri: "mega://cram",
        name: "CRAM",
        description: "128 bytes of colour RAM (4 palette lines × 16 colours × 9-bit BGR).",
        mime_type: "application/octet-stream",
        source: ResourceSource::Region(MemorySpace::Cram),
    },
    ResourceDef {
        uri: "mega://vsram",
        name: "VSRAM",
        description: "80 bytes of vertical scroll RAM.",
        mime_type: "application/octet-stream",
        source: ResourceSource::Region(MemorySpace::Vsram),
    },
    ResourceDef {
        uri: "mega://vdp/registers",
        name: "VDP registers",
        description: "VDP register file (24 regs) + decoded plane / sprite / hscroll bases.",
        mime_type: "application/json",
        source: ResourceSource::VdpRegistersJson,
    },
    ResourceDef {
        uri: "mega://sprites",
        name: "Sprite attribute table",
        description: "Decoded sprite list walked from VDP register #5.",
        mime_type: "application/json",
        source: ResourceSource::SpritesJson,
    },
    ResourceDef {
        uri: "mega://m68k/registers",
        name: "68k registers",
        description: "D0..D7, A0..A7, PC, SR, USP, SSP from the m68k_state blob.",
        mime_type: "application/json",
        source: ResourceSource::M68kJson,
    },
    ResourceDef {
        uri: "mega://framebuffer",
        name: "Framebuffer",
        description: "PNG-encoded current framebuffer (M3).",
        mime_type: "image/png",
        source: ResourceSource::FramebufferPng,
    },
    ResourceDef {
        uri: "mega://z80/registers",
        name: "Z80 registers",
        description: "Decoded Z80 register snapshot (AF/BC/DE/HL/IX/IY/PC/SP + IFF/IM/cycles + bus state).",
        mime_type: "application/json",
        source: ResourceSource::Z80Json,
    },
    ResourceDef {
        uri: "mega://breakpoints",
        name: "Breakpoints",
        description: "Live list of registered breakpoints (id, addr, kind, space, hit_count, enabled).",
        mime_type: "application/json",
        source: ResourceSource::BreakpointsJson,
    },
];

pub fn list_resources() -> Vec<Resource> {
    CATALOGUE
        .iter()
        .map(|r| {
            RawResource::new(r.uri, r.name)
                .with_description(r.description)
                .with_mime_type(r.mime_type)
                .no_annotation()
        })
        .collect()
}

pub fn find(uri: &str) -> Option<&'static ResourceDef> {
    CATALOGUE.iter().find(|r| r.uri == uri)
}

/// Look up a cached payload published by the emulator broadcast pump.
fn cached(cache: Option<&SnapshotCache>, uri: &str) -> Option<Vec<u8>> {
    let c = cache?;
    let g = c.read();
    g.get(uri).map(|(b, _)| b.to_vec())
}

pub async fn read_contents(
    actor: &EmulatorActor,
    def: &ResourceDef,
    cache: Option<&SnapshotCache>,
) -> ResourceContents {
    match def.source {
        ResourceSource::Region(space) => {
            let bytes = match cached(cache, def.uri) {
                Some(b) if !b.is_empty() => b,
                _ => actor.snapshot_region(space).await.unwrap_or_default(),
            };
            ResourceContents::BlobResourceContents {
                uri: def.uri.into(),
                mime_type: Some(def.mime_type.into()),
                blob: B64.encode(&bytes),
                meta: None,
            }
        }
        ResourceSource::VdpRegistersJson => {
            // Cached JSON shortcut: the broadcast pump publishes the decoded
            // JSON as the payload for this URI, so when present it's already
            // the final text — return it as text directly.
            if let Some(b) = cached(cache, def.uri) {
                if !b.is_empty() {
                    if let Ok(s) = String::from_utf8(b) {
                        return ResourceContents::TextResourceContents {
                            uri: def.uri.into(),
                            mime_type: Some(def.mime_type.into()),
                            text: s,
                            meta: None,
                        };
                    }
                }
            }
            let blob = actor
                .snapshot_region(MemorySpace::VdpState)
                .await
                .unwrap_or_default();
            let regs = decode::decode_vdp_registers(&blob);
            ResourceContents::TextResourceContents {
                uri: def.uri.into(),
                mime_type: Some(def.mime_type.into()),
                text: serde_json::to_string(&regs).unwrap_or_else(|_| "{}".into()),
                meta: None,
            }
        }
        ResourceSource::SpritesJson => {
            let vdp = actor
                .snapshot_region(MemorySpace::VdpState)
                .await
                .unwrap_or_default();
            let regs = decode::decode_vdp_registers(&vdp);
            let vram = actor
                .snapshot_region(MemorySpace::Vram)
                .await
                .unwrap_or_default();
            let sprites = decode::decode_sprites(&vram, regs.decoded.sprite_table, 80);
            ResourceContents::TextResourceContents {
                uri: def.uri.into(),
                mime_type: Some(def.mime_type.into()),
                text: serde_json::to_string(&sprites).unwrap_or_else(|_| "[]".into()),
                meta: None,
            }
        }
        ResourceSource::M68kJson => {
            let blob = actor
                .snapshot_region(MemorySpace::M68kState)
                .await
                .unwrap_or_default();
            let regs = decode::decode_m68k(&blob).unwrap_or_default();
            ResourceContents::TextResourceContents {
                uri: def.uri.into(),
                mime_type: Some(def.mime_type.into()),
                text: serde_json::to_string(&regs).unwrap_or_else(|_| "{}".into()),
                meta: None,
            }
        }
        ResourceSource::Z80Json => {
            // Cached JSON if the broadcast pump has published it.
            if let Some(b) = cached(cache, def.uri) {
                if !b.is_empty() {
                    if let Ok(s) = String::from_utf8(b) {
                        return ResourceContents::TextResourceContents {
                            uri: def.uri.into(),
                            mime_type: Some(def.mime_type.into()),
                            text: s,
                            meta: None,
                        };
                    }
                }
            }
            let state_blob = actor
                .snapshot_region(MemorySpace::Z80)
                .await
                .unwrap_or_default();
            let bus_blob = actor
                .snapshot_region(MemorySpace::Z80Bus)
                .await
                .unwrap_or_default();
            let regs = decode::decode_z80(&state_blob, &bus_blob).unwrap_or_default();
            ResourceContents::TextResourceContents {
                uri: def.uri.into(),
                mime_type: Some(def.mime_type.into()),
                text: serde_json::to_string(&regs).unwrap_or_else(|_| "{}".into()),
                meta: None,
            }
        }
        ResourceSource::BreakpointsJson => {
            let snap = actor.breakpoints().snapshot();
            ResourceContents::TextResourceContents {
                uri: def.uri.into(),
                mime_type: Some(def.mime_type.into()),
                text: serde_json::to_string(&snap.entries).unwrap_or_else(|_| "[]".into()),
                meta: None,
            }
        }
        ResourceSource::FramebufferPng => {
            // Prefer the most recent broadcast payload (already PNG-encoded).
            if let Some(b) = cached(cache, def.uri) {
                return ResourceContents::BlobResourceContents {
                    uri: def.uri.into(),
                    mime_type: Some(def.mime_type.into()),
                    blob: B64.encode(&b),
                    meta: None,
                };
            }
            // Fall back to a one-shot capture via the actor.
            let png = match actor.screenshot().await {
                Ok(Some(frame)) => {
                    let rgba = crate::emulator::frame::to_rgba8(&frame);
                    crate::emulator::frame::rgba8_to_png(&rgba, frame.w, frame.h)
                        .unwrap_or_default()
                }
                _ => Vec::new(),
            };
            ResourceContents::BlobResourceContents {
                uri: def.uri.into(),
                mime_type: Some(def.mime_type.into()),
                blob: B64.encode(&png),
                meta: None,
            }
        }
    }
}
