// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The QRMI adapter: this crate's single point of contact with QRMI.
//!
//! Confining QRMI here is the point. Its Rust API is pre-1.0 and evolving --
//! `is_accessible()` is already deprecated in favour of `status()`, a drift
//! the Python code absorbs with a `hasattr()` probe at runtime -- so every
//! use of the crate is kept in one place where a version bump can be dealt
//! with once.
//!
//! [`target`] parses the `target()` payload and needs no QRMI dependency, so
//! it is always available. `provider` and `lease` call QRMI and are
//! gated behind the `qrmi` feature, which keeps the rest of this crate --
//! and its tests -- buildable without the QRMI toolchain or a network.

pub mod resource_type;
pub mod target;

#[cfg(feature = "qrmi")]
pub mod lease;
#[cfg(feature = "qrmi")]
pub mod provider;

pub use resource_type::ResourceTypeName;
pub use target::{SystemMedians, Target};

/// The QRMI crate itself, re-exported.
///
/// The binaries construct a `QuantumResource` directly, so they need the
/// QRMI types. Re-exporting here rather than letting each binary depend on
/// QRMI keeps the version pin in one manifest: a binary cannot accidentally
/// link a different QRMI than the adapter was compiled against.
#[cfg(feature = "qrmi")]
pub use ::qrmi as qrmi_crate;
