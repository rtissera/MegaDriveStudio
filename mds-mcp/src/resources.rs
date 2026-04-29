// SPDX-License-Identifier: MIT
//! `mega://*` resource catalogue + read/subscribe routing.
//!
//! The catalogue is the authoritative list returned to `resources/list` and
//! used to validate `resources/read` URIs. Subscriptions are tracked in a
//! `parking_lot::Mutex<HashSet<String>>` per `MdsServer`; subscriber-side
//! delivery happens on the broadcast channel exposed by `EmulatorActor`.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rmcp::model::{AnnotateAble, RawResource, Resource, ResourceContents};

use crate::emulator::{decode, EmulatorActor, MemorySpace};

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

pub async fn read_contents(actor: &EmulatorActor, def: &ResourceDef) -> ResourceContents {
    match def.source {
        ResourceSource::Region(space) => {
            let bytes = actor.snapshot_region(space).await.unwrap_or_default();
            ResourceContents::BlobResourceContents {
                uri: def.uri.into(),
                mime_type: Some(def.mime_type.into()),
                blob: B64.encode(&bytes),
                meta: None,
            }
        }
        ResourceSource::VdpRegistersJson => {
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
        ResourceSource::FramebufferPng => ResourceContents::BlobResourceContents {
            uri: def.uri.into(),
            mime_type: Some(def.mime_type.into()),
            // Empty PNG sentinel until M3.
            blob: B64.encode([] as [u8; 0]),
            meta: None,
        },
    }
}
