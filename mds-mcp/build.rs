// SPDX-License-Identifier: MIT
//! Build script: generates Rust FFI bindings for libra (../vendor/libra),
//! and ensures the on-cart 68k debug stub blob (`../mds-stub-68k/mdsstub.bin`)
//! is built so that `target::edpro::stub_blob::STUB_BLOB`'s
//! `include_bytes!` succeeds.
//!
//! When the libra header / library aren't yet available (the parallel agent
//! hasn't finished), we still emit a *complete* but synthetic stub
//! `bindings.rs` so the rest of the crate compiles. We expose the cfg
//! `libra_present` only when bindgen produced real bindings; downstream
//! modules use `#[cfg(libra_present)]` to switch between live FFI and the
//! safe-no-op fallback.
//!
//! For the 68k stub: if `m68k-elf-gcc` isn't on PATH and the marsdev
//! toolchain isn't at `~/mars`, we write a 32-byte placeholder blob with
//! the correct magic + entries pointing back into the placeholder so the
//! Rust crate still compiles. This keeps `cargo check` working in
//! environments without the cross-toolchain. Production builds set
//! `MDS_STUB_REQUIRE_REAL=1` to fail loud on placeholder fallback.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const STUB_BINDINGS: &str = r#"// SPDX-License-Identifier: MIT
// Stub bindings — libra header was not present at build time.
// Run `git submodule update --init --recursive` then rebuild.

#![allow(dead_code)]

/// Opaque libra context type (stub).
#[repr(C)]
pub struct libra_ctx {
    _private: [u8; 0],
}

/// Stub of `libra_config_t`. Exists so the rest of the crate type-checks.
#[repr(C)]
#[derive(Default)]
pub struct libra_config_t {
    pub _opaque: [usize; 32],
}
"#;

/// Synthetic 40-byte placeholder blob, used when the real m68k toolchain
/// isn't available so `cargo check` / `cargo build` still succeed.
///
/// Layout matches `parse_header` in `target::edpro::stub_blob` (24-byte
/// header):
///   +0x00 MAGIC ('MDST')
///   +0x04 entry_trace
///   +0x08 entry_trap1
///   +0x0C entry_vbl
///   +0x10 paused_flag   (must be 0)
///   +0x14 original_vbl  (must be 0)
///   +0x18 .. NOPs (`0x4E71` repeated) as the placeholder body.
/// Entry points sit inside the body so they land in NOPs rather than off
/// the end of the blob — but they are deliberately distinct to mirror a
/// real build, where the trace / trap1 / vbl handlers live at different
/// offsets. Anyone actually running this on hardware will spin in NOPs
/// forever, which is the desired loud failure mode.
const PLACEHOLDER_LOAD_ADDR: u32 = 0x0030_0000;
const PLACEHOLDER_ENTRY_TRACE: u32 = PLACEHOLDER_LOAD_ADDR + 0x18; // first NOP
const PLACEHOLDER_ENTRY_TRAP1: u32 = PLACEHOLDER_LOAD_ADDR + 0x1C; // third NOP
const PLACEHOLDER_ENTRY_VBL: u32 = PLACEHOLDER_LOAD_ADDR + 0x20; // fifth NOP

fn write_placeholder_stub(out: &Path) -> std::io::Result<()> {
    let mut blob = Vec::with_capacity(40);
    blob.extend_from_slice(&0x4D44_5354u32.to_be_bytes()); // 'MDST'
    blob.extend_from_slice(&PLACEHOLDER_ENTRY_TRACE.to_be_bytes());
    blob.extend_from_slice(&PLACEHOLDER_ENTRY_TRAP1.to_be_bytes());
    blob.extend_from_slice(&PLACEHOLDER_ENTRY_VBL.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes()); // paused_flag
    blob.extend_from_slice(&0u32.to_be_bytes()); // original_vbl
    for _ in 0..8 {
        blob.extend_from_slice(&0x4E71u16.to_be_bytes()); // NOP
    }
    fs::write(out, &blob)
}

fn build_stub_blob(stub_dir: &Path) -> bool {
    // Try `make` against the stub Makefile. We deliberately don't pass -j
    // because the build is tiny and noise-free.
    let status = Command::new("make")
        .current_dir(stub_dir)
        .arg("all")
        .status();
    match status {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

fn ensure_stub_blob(manifest_dir: &Path) {
    let stub_dir = manifest_dir.join("../mds-stub-68k");
    let bin = stub_dir.join("mdsstub.bin");

    println!("cargo:rerun-if-changed={}", stub_dir.join("Makefile").display());
    println!("cargo:rerun-if-changed={}", stub_dir.join("mdsstub.ld").display());
    if let Ok(entries) = fs::read_dir(stub_dir.join("src")) {
        for e in entries.flatten() {
            println!("cargo:rerun-if-changed={}", e.path().display());
        }
    }
    println!("cargo:rerun-if-env-changed=MDS_STUB_REQUIRE_REAL");

    let require_real = env::var("MDS_STUB_REQUIRE_REAL").is_ok();

    if stub_dir.exists() && build_stub_blob(&stub_dir) && bin.exists() {
        // Real build succeeded.
        return;
    }

    if require_real {
        panic!(
            "MDS_STUB_REQUIRE_REAL is set but mds-stub-68k build failed (m68k-elf-gcc missing or stub source error). Run `make -C mds-stub-68k` manually to debug."
        );
    }

    // Fall back to a placeholder so `cargo check` works on machines without
    // the m68k toolchain. The Rust side surfaces this clearly: STUB_BLOB
    // will still parse, but the entry points point at the placeholder
    // itself rather than real handler code.
    println!("cargo:warning=mds-stub-68k toolchain unavailable; writing placeholder mdsstub.bin (set MDS_STUB_REQUIRE_REAL=1 to enforce real build)");
    if let Err(e) = write_placeholder_stub(&bin) {
        panic!("failed to write placeholder stub blob to {}: {e}", bin.display());
    }
}

fn main() {
    println!("cargo::rustc-check-cfg=cfg(libra_present)");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    ensure_stub_blob(&manifest_dir);
    let header = manifest_dir.join("../vendor/libra/include/libra.h");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let bindings_out = out_dir.join("bindings.rs");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", header.display());

    if !header.exists() {
        println!(
            "cargo:warning=libra header not found at {}. Writing stub bindings; libra-backed features will be no-ops.",
            header.display()
        );
        fs::write(&bindings_out, STUB_BINDINGS).expect("write stub bindings");
        return;
    }

    let include_dir = manifest_dir.join("../vendor/libra/include");
    let src_dir = manifest_dir.join("../vendor/libra/src");
    let deps_dir = manifest_dir.join("../vendor/libra/deps");

    let builder = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .clang_arg(format!("-I{}", include_dir.display()))
        .clang_arg(format!("-I{}", src_dir.display()))
        .clang_arg(format!("-I{}", deps_dir.display()))
        .allowlist_function("libra_.*")
        .allowlist_type("libra_.*")
        .allowlist_var("RETRO_MEMORY_.*")
        .allowlist_var("LIBRA_.*")
        .generate_comments(true)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    let bindings = match builder.generate() {
        Ok(b) => b,
        Err(e) => {
            println!(
                "cargo:warning=bindgen failed: {e}. Writing stub bindings; libra-backed features will be no-ops."
            );
            fs::write(&bindings_out, STUB_BINDINGS).expect("write stub bindings");
            return;
        }
    };

    bindings
        .write_to_file(&bindings_out)
        .expect("failed to write bindings.rs");

    // Successful bindgen — switch on libra_present and link.
    println!("cargo:rustc-cfg=libra_present");

    let lib_dir = manifest_dir.join("../vendor/libra/build");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    let lib_dir_release = manifest_dir.join("../vendor/libra/build_release");
    println!("cargo:rustc-link-search=native={}", lib_dir_release.display());

    let lib_name = env::var("LIBRA_LIB_NAME").unwrap_or_else(|_| "libra".to_string());
    println!("cargo:rustc-link-lib={lib_name}");
}
