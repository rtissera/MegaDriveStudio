// SPDX-License-Identifier: MIT
//! Build script: generates Rust FFI bindings for libra (../vendor/libra).
//!
//! If `../vendor/libra/include/libra.h` is missing (the parallel agent has not
//! initialised submodules yet), we emit a `cargo:warning` and fall back to a
//! stub `bindings.rs` so that `cargo check` / `cargo build` succeed for
//! downstream tooling. Linking will then fail at the actual link stage with a
//! clear message — that's fine, M1 is a scaffold.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let header = manifest_dir.join("../vendor/libra/include/libra.h");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let bindings_out = out_dir.join("bindings.rs");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", header.display());

    if !header.exists() {
        println!(
            "cargo:warning=libra header not found at {}. Run `git submodule update --init --recursive` from the repo root. Writing stub bindings; linking will fail later (expected during scaffold).",
            header.display()
        );
        let stub = "// SPDX-License-Identifier: MIT\n// Stub bindings — libra header was missing at build time.\n// Run `git submodule update --init --recursive` then rebuild.\n";
        fs::write(&bindings_out, stub).expect("write stub bindings");
        return;
    }

    let include_dir = manifest_dir.join("../vendor/libra/include");
    let src_dir = manifest_dir.join("../vendor/libra/src");
    // libra.h transitively includes libra_internal.h, which #include "libretro.h".
    // The vendored libretro header lives in vendor/libra/deps. Without this
    // include path, bindgen falls back to stubs.
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
                "cargo:warning=bindgen failed against libra header: {e}. Writing stub bindings; the parallel libra agent may not have populated all transitive headers yet. Linking will fail later (expected)."
            );
            let stub = format!(
                "// SPDX-License-Identifier: MIT\n// Stub bindings — bindgen failed: {e}\n"
            );
            fs::write(&bindings_out, stub).expect("write stub bindings");
            // Still emit link search hints in case the C lib happens to be present.
            let lib_dir = manifest_dir.join("../vendor/libra/build");
            println!("cargo:rustc-link-search=native={}", lib_dir.display());
            return;
        }
    };

    bindings
        .write_to_file(&bindings_out)
        .expect("failed to write bindings.rs");

    // Tell rustc where to find the static/shared lib produced by libra's CMake.
    let lib_dir = manifest_dir.join("../vendor/libra/build");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    let lib_dir_release = manifest_dir.join("../vendor/libra/build_release");
    println!("cargo:rustc-link-search=native={}", lib_dir_release.display());

    // Allow override via env var (LIBRA_LIB_NAME=libra_static, etc).
    let lib_name = env::var("LIBRA_LIB_NAME").unwrap_or_else(|_| "libra".to_string());
    println!("cargo:rustc-link-lib={}", lib_name);
}
