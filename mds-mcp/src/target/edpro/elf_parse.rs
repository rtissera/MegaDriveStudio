// SPDX-License-Identifier: MIT
//! M5.8a — extract SGDK VDP-state shadow symbol addresses from a debug ELF.
//!
//! VDP registers `$00..$17` are *write-only* on the Mega Drive — the host
//! has no MMIO path to read them back. Same for the Plane A/B/Window/SAT
//! base addresses. SGDK keeps a software shadow of every value the user
//! code ever wrote, in plain MD work RAM (`$00FF_xxxx`). Those shadow
//! globals are visible in any non-stripped ELF's `.symtab`:
//!
//! - `regValues` — 19 bytes — every byte written via `VDP_setReg`.
//! - `bga_addr` / `bgb_addr` / `slist_addr` / `window_addr` / `hscroll`
//!   — `u16` each — the VRAM offsets the planes / SAT / window / hscroll
//!   table currently live at (latest values written via `VDP_setBGA…`).
//! - `palette_cache` — name varies by SGDK version (`palette_cache`,
//!   `paletteCache`, `palette`); optional, used for screenshot
//!   reconstruction in M5.8b.
//!
//! Once the host knows those addresses it issues plain RSP `m` reads
//! against work RAM through the on-cart 68k stub — same path used for
//! every other `mega_read_memory` call.
//!
//! We only parse `.symtab` (the static-link symbol table). `.dynsym` is
//! useless: SGDK's link is fully static and these are local symbols.

#![allow(dead_code)] // wired into EdProTarget in this same milestone, but
                    // some helpers are public for the tool layer's later use.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use object::elf::FileHeader32;
use object::read::elf::{ElfFile, FileHeader, Sym};
use object::Endianness;

/// Resolved work-RAM addresses of SGDK's VDP-state shadow globals.
///
/// All addresses live in MD work RAM (`$00FF_0000..$0100_0000` mirrored).
/// The on-cart 68k debug stub services RSP `m` packets via the shared bus,
/// so reads against these addresses Just Work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgdkSymbols {
    /// `regValues[0..19]` — software mirror of VDP regs `$00..$12`.
    pub reg_values: u32,
    /// `bga_addr` — Plane A VRAM base offset (u16).
    pub bga_addr: u32,
    /// `bgb_addr` — Plane B VRAM base offset (u16).
    pub bgb_addr: u32,
    /// `slist_addr` — Sprite Attribute Table VRAM base offset (u16).
    pub slist_addr: u32,
    /// `window_addr` — Window plane VRAM base offset (u16).
    pub window_addr: u32,
    /// `hscroll` — HScroll table VRAM base offset (u16).
    pub hscroll: u32,
    /// Optional CRAM/palette cache shadow. Name and presence vary across
    /// SGDK versions; `None` means none of the candidate names matched.
    pub palette_cache: Option<u32>,
}

/// Required symbols — every one of these MUST be present, otherwise the
/// VDP-state path can't be wired up.
const REQUIRED: &[&str] = &[
    "regValues",
    "bga_addr",
    "bgb_addr",
    "slist_addr",
    "window_addr",
    "hscroll",
];

/// Optional palette-cache symbol — try these names in order, take the
/// first match. None of them present → `palette_cache: None`.
const PALETTE_CANDIDATES: &[&str] = &["palette_cache", "paletteCache", "palette"];

/// Parse an m68k debug ELF and resolve SGDK's VDP-state shadow addresses.
///
/// Reads `.symtab` (NOT `.dynsym` — these are local symbols emitted by the
/// linker for static binaries). On success, every required address is
/// returned; `palette_cache` is `Some` iff one of [`PALETTE_CANDIDATES`]
/// was found.
///
/// Returns an error if any required symbol is missing — the message
/// explains how the user can fix it (rebuild with `make debug`, don't
/// run `m68k-elf-strip`).
pub fn parse_sgdk_symbols(elf_path: &Path) -> Result<SgdkSymbols> {
    let bytes = std::fs::read(elf_path)
        .with_context(|| format!("failed to read ELF at {}", elf_path.display()))?;
    parse_sgdk_symbols_from_bytes(&bytes)
        .with_context(|| format!("ELF parse failed for {}", elf_path.display()))
}

/// In-memory variant — handy for unit tests and for callers that already
/// have the bytes mmaped.
pub fn parse_sgdk_symbols_from_bytes(bytes: &[u8]) -> Result<SgdkSymbols> {
    // m68k targets are always 32-bit big-endian. We type-erase the endian
    // here (Endianness::Big) so the parser still validates `e_ident[5]`
    // matches at runtime — a little-endian ELF would error out cleanly.
    let elf: ElfFile<FileHeader32<Endianness>> =
        ElfFile::parse(bytes).map_err(|e| anyhow!("not a valid ELF: {e}"))?;

    let header = elf.elf_header();
    let endian = header.endian().map_err(|e| anyhow!("ELF endian probe: {e}"))?;
    let data = elf.data();

    // Locate .symtab + its associated string table.
    let sections = header
        .sections(endian, data)
        .map_err(|e| anyhow!("ELF sections: {e}"))?;
    let symtab = sections
        .symbols(endian, data, object::elf::SHT_SYMTAB)
        .map_err(|e| anyhow!("ELF .symtab: {e}"))?;
    if symtab.is_empty() {
        return Err(anyhow!(
            ".symtab is empty — ROM was likely stripped. Don't run m68k-elf-strip; \
             ensure `make debug` keeps `-g` and the symbol table."
        ));
    }

    // Walk every symbol; collect candidates by name. We don't filter by
    // type / binding — SGDK's globals show up as STT_OBJECT with various
    // bindings depending on whether the symbol was static-local
    // (`regValues`, `hscroll` are file-local, STB_LOCAL) or extern
    // (`bga_addr`, `slist_addr`, ... STB_GLOBAL).
    let mut reg_values: Option<u32> = None;
    let mut bga_addr: Option<u32> = None;
    let mut bgb_addr: Option<u32> = None;
    let mut slist_addr: Option<u32> = None;
    let mut window_addr: Option<u32> = None;
    let mut hscroll: Option<u32> = None;
    // Palette: track best candidate by priority order (lower idx = higher).
    let mut palette: Option<(usize, u32)> = None;

    for sym in symtab.iter() {
        let name_bytes = match symtab.symbol_name(endian, sym) {
            Ok(n) => n,
            Err(_) => continue,
        };
        if name_bytes.is_empty() {
            continue;
        }
        let name = match std::str::from_utf8(name_bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let value = sym.st_value(endian);

        match name {
            "regValues" => reg_values = Some(value),
            "bga_addr" => bga_addr = Some(value),
            "bgb_addr" => bgb_addr = Some(value),
            "slist_addr" => slist_addr = Some(value),
            "window_addr" => window_addr = Some(value),
            "hscroll" => hscroll = Some(value),
            other => {
                if let Some(idx) = PALETTE_CANDIDATES.iter().position(|c| *c == other) {
                    // Keep highest-priority (lowest index) candidate.
                    let take = match palette {
                        Some((cur_idx, _)) => idx < cur_idx,
                        None => true,
                    };
                    if take {
                        palette = Some((idx, value));
                    }
                }
            }
        }
    }

    let mut missing: Vec<&'static str> = Vec::new();
    if reg_values.is_none() {
        missing.push("regValues");
    }
    if bga_addr.is_none() {
        missing.push("bga_addr");
    }
    if bgb_addr.is_none() {
        missing.push("bgb_addr");
    }
    if slist_addr.is_none() {
        missing.push("slist_addr");
    }
    if window_addr.is_none() {
        missing.push("window_addr");
    }
    if hscroll.is_none() {
        missing.push("hscroll");
    }
    if !missing.is_empty() {
        return Err(anyhow!(
            "missing required SGDK symbols in .symtab: {missing:?}. \
             ROM not built with debug? Try `make debug`. Stripped? \
             Don't run m68k-elf-strip — `.symtab` is required.",
            missing = missing,
        ));
    }

    Ok(SgdkSymbols {
        reg_values: reg_values.unwrap(),
        bga_addr: bga_addr.unwrap(),
        bgb_addr: bgb_addr.unwrap(),
        slist_addr: slist_addr.unwrap(),
        window_addr: window_addr.unwrap(),
        hscroll: hscroll.unwrap(),
        palette_cache: palette.map(|(_, v)| v),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::write::elf::{FileHeader, Sym, Writer};
    use object::write::StreamingBuffer;
    use object::elf as e;

    /// Build a minimal m68k BE ELF (relocatable) with the given symbol set
    /// stuffed into `.symtab`. We don't need any real code/data — every
    /// symbol can sit in a synthetic `.bss` section, or `SHN_ABS` if we
    /// want to dodge sections entirely. We use an absolute section ref
    /// here for simplicity: the parser only cares about `st_value`.
    fn build_elf(syms: &[(&str, u32)]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut sb = StreamingBuffer::new(&mut buf);
        let mut w = Writer::new(Endianness::Big, false, &mut sb);

        // Reserve all section + symbol indices first (object::write's
        // two-phase pattern: reserve, then write in the same order).
        w.reserve_null_section_index();
        // We don't add any "real" payload sections — `.symtab`,
        // `.strtab`, `.shstrtab` are all the writer needs.
        let symtab_index = w.reserve_symtab_section_index();
        let _strtab_index = w.reserve_strtab_section_index();
        let _shstrtab_index = w.reserve_shstrtab_section_index();

        // Reserve each symbol. Local + STT_OBJECT, sized 0, bound to
        // SHN_ABS so we don't need a real section.
        let null_sym = w.reserve_null_symbol_index();
        let _ = null_sym;
        let mut sym_indices = Vec::with_capacity(syms.len());
        let mut name_strids = Vec::with_capacity(syms.len());
        for (name, _value) in syms {
            let nid = w.add_string(name.as_bytes());
            name_strids.push(nid);
            sym_indices.push(w.reserve_symbol_index(None));
        }

        // File header + section header offsets — needed before we can
        // write the ELF header itself.
        w.reserve_file_header();
        w.reserve_symtab();
        w.reserve_strtab();
        w.reserve_shstrtab();
        w.reserve_section_headers();

        // ----- write phase -----
        w.write_file_header(&FileHeader {
            os_abi: e::ELFOSABI_NONE,
            abi_version: 0,
            // ET_REL is fine; we're not loading.
            e_type: e::ET_REL,
            // EM_68K = 4. m68k.
            e_machine: e::EM_68K,
            e_entry: 0,
            e_flags: 0,
        })
        .unwrap();

        w.write_null_symbol();
        for ((_, value), nid) in syms.iter().zip(name_strids.iter()) {
            w.write_symbol(&Sym {
                name: Some(*nid),
                section: None, // SHN_ABS via st_shndx=0xFFF1 — `Writer`
                               // emits SHN_ABS automatically when section
                               // is None and we're not flagged as common.
                st_info: (e::STB_LOCAL << 4) | e::STT_OBJECT,
                st_other: e::STV_DEFAULT,
                st_shndx: e::SHN_ABS,
                st_value: *value as u64,
                st_size: 0,
            });
        }
        w.write_strtab();
        w.write_shstrtab();
        w.write_null_section_header();
        w.write_symtab_section_header(1); // first non-null sym idx
        w.write_strtab_section_header();
        w.write_shstrtab_section_header();

        let _ = symtab_index;
        buf
    }

    fn full_required() -> Vec<(&'static str, u32)> {
        vec![
            ("regValues", 0x00FF_AAAA),
            ("bga_addr", 0x00FF_BBBB),
            ("bgb_addr", 0x00FF_CCCC),
            ("slist_addr", 0x00FF_DDDD),
            ("window_addr", 0x00FF_EEEE),
            ("hscroll", 0x00FF_FFF0),
        ]
    }

    #[test]
    fn parses_required_symbols_round_trip() {
        let bytes = build_elf(&full_required());
        let s = parse_sgdk_symbols_from_bytes(&bytes).unwrap();
        assert_eq!(s.reg_values, 0x00FF_AAAA);
        assert_eq!(s.bga_addr, 0x00FF_BBBB);
        assert_eq!(s.bgb_addr, 0x00FF_CCCC);
        assert_eq!(s.slist_addr, 0x00FF_DDDD);
        assert_eq!(s.window_addr, 0x00FF_EEEE);
        assert_eq!(s.hscroll, 0x00FF_FFF0);
        assert_eq!(s.palette_cache, None);
    }

    #[test]
    fn missing_required_symbol_errors_with_helpful_message() {
        // Drop `slist_addr` — must surface in the error string and the
        // hint about `make debug` / `m68k-elf-strip` must be present.
        let mut syms = full_required();
        syms.retain(|(n, _)| *n != "slist_addr");
        let bytes = build_elf(&syms);
        let err = parse_sgdk_symbols_from_bytes(&bytes).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("slist_addr"), "missing list must include slist_addr: {msg}");
        assert!(
            msg.contains("make debug") || msg.contains("strip"),
            "error must hint at the fix, got: {msg}"
        );
    }

    #[test]
    fn palette_cache_optional_first_candidate_wins() {
        let mut syms = full_required();
        // Add the second-priority candidate and the first one — first
        // wins regardless of order in the symbol table.
        syms.push(("paletteCache", 0x00FF_2222));
        syms.push(("palette_cache", 0x00FF_1111));
        let bytes = build_elf(&syms);
        let s = parse_sgdk_symbols_from_bytes(&bytes).unwrap();
        assert_eq!(s.palette_cache, Some(0x00FF_1111));
    }

    #[test]
    fn palette_cache_falls_back_to_lower_priority() {
        let mut syms = full_required();
        // Only the third-priority candidate is present.
        syms.push(("palette", 0x00FF_3333));
        let bytes = build_elf(&syms);
        let s = parse_sgdk_symbols_from_bytes(&bytes).unwrap();
        assert_eq!(s.palette_cache, Some(0x00FF_3333));
    }

    #[test]
    fn no_palette_symbol_means_none() {
        let bytes = build_elf(&full_required());
        let s = parse_sgdk_symbols_from_bytes(&bytes).unwrap();
        assert_eq!(s.palette_cache, None);
    }

    #[test]
    fn not_an_elf_errors_cleanly() {
        let err = parse_sgdk_symbols_from_bytes(b"this is not an ELF").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("ELF") || msg.contains("not a valid"), "got: {msg}");
    }
}
