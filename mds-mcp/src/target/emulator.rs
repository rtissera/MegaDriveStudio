// SPDX-License-Identifier: MIT
//! Emulator target — thin shim around the existing in-process `EmulatorActor`.
//!
//! For M5-prep we keep the actor in place (resources, notifier, framebuffer
//! channel all hang off it) and just expose its kind via the `Target` enum
//! the CLI selects. A future refactor can hide the actor entirely behind a
//! `dyn Target` once the trait surface stabilises.

use crate::emulator::EmulatorActor;
use crate::target::TargetKind;

#[allow(dead_code)] // wrapper kept for future trait-based dispatch
pub struct EmulatorTarget {
    pub actor: EmulatorActor,
}

#[allow(dead_code)]
impl EmulatorTarget {
    pub fn new(actor: EmulatorActor) -> Self {
        Self { actor }
    }

    pub fn kind(&self) -> TargetKind {
        TargetKind::Emulator
    }
}
