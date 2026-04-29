// SPDX-License-Identifier: MIT
//! Build script: generates Rust FFI bindings for libra (../vendor/libra).
//!
//! When the libra header / library aren't yet available (the parallel agent
//! hasn't finished), we still emit a *complete* but synthetic stub
//! `bindings.rs` so the rest of the crate compiles. We expose the cfg
//! `libra_present` only when bindgen produced real bindings; downstream
//! modules use `#[cfg(libra_present)]` to switch between live FFI and the
//! safe-no-op fallback.

use std::env;
use std::fs;
use std::path::PathBuf;

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

fn main() {
    println!("cargo::rustc-check-cfg=cfg(libra_present)");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
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
