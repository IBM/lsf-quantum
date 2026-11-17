// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The LSF scheduler protocol surface.
//!
//! Each submodule covers one interface with LSF, isolated so it can be
//! tested against a temporary file or a stub command rather than a running
//! cluster:
//!
//! * [`esub`] — the `LSF_SUB4_SUB_ENV_VARS` directive written to
//!   `$LSB_SUB_MODIFY_FILE`;
//! * [`bpost`] — posting the QRMI acquisition ID to the job record;
//! * [`elim`] — the load-index line format;
//! * [`hostres`] — mapping an ELIM host to the QPU it serves.

pub mod bpost;
pub mod elim;
pub mod esub;
pub mod hostres;
