// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! C ABI over [`qpu_select`], for the LSF scheduler plug-in (`schmod_qrmi`).
//!
//! The plug-in runs inside `mbschd`, so two rules govern this crate:
//!
//! * **Nothing unwinds into C.** A panic in `mbschd` would stop all
//!   scheduling in the cluster; every entry point catches it and reports an
//!   error instead.
//! * **No I/O, no allocation the caller must free.** Results are copied into
//!   caller-supplied buffers.

use std::ffi::{c_char, c_int, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};

use qpu_select::{selector_by_name, Candidates, ProcessorType, QpuMetrics, Request, Selection};

/// A candidate QPU as the C plug-in describes it. Mirrors `qrmi_select.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct QrmiQpu {
    /// QPU name (borrowed, NUL-terminated).
    pub name: *const c_char,
    /// Qubits; negative if unknown.
    pub qubits: i64,
    /// Queue depth; negative if unknown.
    pub pending_jobs: i64,
    /// CLOPS; NaN if unknown.
    pub clops: f64,
    /// Median readout error; NaN if unknown.
    pub readout_error_median: f64,
    /// Median SX error; NaN if unknown.
    pub sx_error_median: f64,
    /// Median CZ error; NaN if unknown.
    pub cz_error_median: f64,
    /// Median T1 in microseconds; NaN if unknown.
    pub t1_median_us: f64,
    /// Median T2 in microseconds; NaN if unknown.
    pub t2_median_us: f64,
    /// QPU version string, or NULL.
    pub qpu_version: *const c_char,
    /// Processor type as the ELIM renders it (`Heron_r2`), or NULL.
    pub processor_type: *const c_char,
    /// Non-zero if the QPU is accepting work.
    pub available: c_int,
}

/// `out` holds the chosen QPU.
pub const QRMI_SELECT_OK: c_int = 0;
/// Nothing satisfied the request.
pub const QRMI_SELECT_NONE: c_int = 1;
/// Bad arguments, selector or request.
pub const QRMI_SELECT_ERROR: c_int = -1;

/// Split the ELIM's `Family_rN` rendering back into family and revision.
///
/// `Heron_r2` becomes (`Heron`, `2`). Anything not of that shape is kept
/// whole as the family.
fn parse_processor_type(text: &str) -> ProcessorType {
    if let Some((family, tail)) = text.rsplit_once('_') {
        if let Some(revision) = tail.strip_prefix('r') {
            if !family.is_empty()
                && !revision.is_empty()
                && revision.bytes().all(|b| b.is_ascii_digit())
            {
                return ProcessorType::new(family).with_revision(revision);
            }
        }
    }
    ProcessorType::new(text)
}

/// Copy `text` into `buffer`, truncating, always NUL-terminating.
///
/// # Safety
///
/// `buffer` must be null or valid for `length` bytes of writes.
unsafe fn write_cstr(buffer: *mut c_char, length: usize, text: &str) {
    if buffer.is_null() || length == 0 {
        return;
    }
    let bytes = text.as_bytes();
    let count = bytes.len().min(length - 1);
    // SAFETY: caller guarantees `length` writable bytes; count < length.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), buffer, count);
        *buffer.add(count) = 0;
    }
}

/// # Safety
///
/// `pointer` must be null or a valid NUL-terminated string.
unsafe fn borrow_str<'a>(pointer: *const c_char) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: caller guarantees a valid NUL-terminated string.
    unsafe { CStr::from_ptr(pointer) }.to_str().ok()
}

/// # Safety
///
/// Every pointer in `qpu` must be null or a valid NUL-terminated string.
unsafe fn convert(qpu: &QrmiQpu) -> Option<(String, QpuMetrics)> {
    // SAFETY: forwarded from this function's contract.
    let name = unsafe { borrow_str(qpu.name) }?.to_owned();

    let mut builder = QpuMetrics::builder().available(qpu.available != 0);

    if let Ok(value) = u32::try_from(qpu.qubits) {
        builder = builder.qubits(value);
    }
    if let Ok(value) = u64::try_from(qpu.pending_jobs) {
        builder = builder.pending_jobs(value);
    }
    for (value, set) in [
        (qpu.clops, QpuMetricsSetter::Clops),
        (qpu.readout_error_median, QpuMetricsSetter::Readout),
        (qpu.sx_error_median, QpuMetricsSetter::Sx),
        (qpu.cz_error_median, QpuMetricsSetter::Cz),
        (qpu.t1_median_us, QpuMetricsSetter::T1),
        (qpu.t2_median_us, QpuMetricsSetter::T2),
    ] {
        if value.is_finite() {
            builder = match set {
                QpuMetricsSetter::Clops => builder.clops(value),
                QpuMetricsSetter::Readout => builder.readout_error_median(value),
                QpuMetricsSetter::Sx => builder.sx_error_median(value),
                QpuMetricsSetter::Cz => builder.cz_error_median(value),
                QpuMetricsSetter::T1 => builder.t1_median_us(value),
                QpuMetricsSetter::T2 => builder.t2_median_us(value),
            };
        }
    }

    let mut metrics = builder.build();
    // SAFETY: forwarded from this function's contract.
    metrics.qpu_version = unsafe { borrow_str(qpu.qpu_version) }.map(str::to_owned);
    // SAFETY: as above.
    metrics.processor_type = unsafe { borrow_str(qpu.processor_type) }.map(parse_processor_type);

    Some((name, metrics))
}

enum QpuMetricsSetter {
    Clops,
    Readout,
    Sx,
    Cz,
    T1,
    T2,
}

fn run(selector: &str, request: &str, candidates: &Candidates) -> Result<Selection, String> {
    let policy =
        selector_by_name(selector).ok_or_else(|| format!("unknown selector: {selector}"))?;
    let parsed =
        Request::parse(request).map_err(|error| format!("cannot parse {request:?}: {error}"))?;
    Ok(policy.select(candidates, &parsed))
}

/// Choose a QPU. See `qrmi_select.h`.
///
/// # Safety
///
/// * `selector` and `request` must be valid NUL-terminated strings.
/// * `qpus` must point to `n_qpus` valid [`QrmiQpu`] values whose string
///   pointers are null or valid NUL-terminated strings.
/// * `out` and `msg` must be null or writable for `out_len` / `msg_len`
///   bytes.
#[no_mangle]
pub unsafe extern "C" fn qrmi_select(
    selector: *const c_char,
    request: *const c_char,
    qpus: *const QrmiQpu,
    n_qpus: usize,
    out: *mut c_char,
    out_len: usize,
    msg: *mut c_char,
    msg_len: usize,
) -> c_int {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: forwarded from this function's contract.
        let (Some(selector), Some(request)) = (unsafe { borrow_str(selector) }, unsafe {
            borrow_str(request)
        }) else {
            return Err("selector and request must be valid strings".to_owned());
        };
        if qpus.is_null() && n_qpus > 0 {
            return Err("null candidate array".to_owned());
        }

        let mut candidates = Candidates::new();
        for index in 0..n_qpus {
            // SAFETY: `qpus` points to `n_qpus` values per the contract.
            let qpu = unsafe { &*qpus.add(index) };
            // SAFETY: the string pointers obey the contract.
            if let Some((name, metrics)) = unsafe { convert(qpu) } {
                candidates.insert(name, metrics);
            }
        }

        run(selector, request, &candidates)
    }));

    match outcome {
        Ok(Ok(selection)) => {
            if let Some(name) = selection.name() {
                // SAFETY: buffer contract is the caller's.
                unsafe { write_cstr(out, out_len, name) };
                QRMI_SELECT_OK
            } else {
                let reasons: Vec<String> = selection
                    .rejected
                    .iter()
                    .map(|rejected| format!("{}: {}", rejected.name, rejected.reason))
                    .collect();
                let text = if reasons.is_empty() {
                    "no candidate QPUs".to_owned()
                } else {
                    reasons.join("; ")
                };
                // SAFETY: buffer contract is the caller's.
                unsafe { write_cstr(msg, msg_len, &text) };
                QRMI_SELECT_NONE
            }
        }
        Ok(Err(text)) => {
            // SAFETY: buffer contract is the caller's.
            unsafe { write_cstr(msg, msg_len, &text) };
            QRMI_SELECT_ERROR
        }
        Err(_) => {
            // SAFETY: buffer contract is the caller's.
            unsafe { write_cstr(msg, msg_len, "internal error in qpu-select") };
            QRMI_SELECT_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    struct Owned {
        name: CString,
        version: CString,
        processor: CString,
    }

    fn qpu(owned: &Owned, qubits: i64, pending: i64, available: bool) -> QrmiQpu {
        QrmiQpu {
            name: owned.name.as_ptr(),
            qubits,
            pending_jobs: pending,
            clops: f64::NAN,
            readout_error_median: f64::NAN,
            sx_error_median: f64::NAN,
            cz_error_median: f64::NAN,
            t1_median_us: f64::NAN,
            t2_median_us: f64::NAN,
            qpu_version: owned.version.as_ptr(),
            processor_type: owned.processor.as_ptr(),
            available: c_int::from(available),
        }
    }

    fn owned(name: &str) -> Owned {
        Owned {
            name: CString::new(name).unwrap(),
            version: CString::new("1.3.29").unwrap(),
            processor: CString::new("Heron_r2").unwrap(),
        }
    }

    fn call(selector: &str, request: &str, qpus: &[QrmiQpu]) -> (c_int, String, String) {
        let selector = CString::new(selector).unwrap();
        let request = CString::new(request).unwrap();
        let mut out = [0 as c_char; 64];
        let mut msg = [0 as c_char; 256];
        // SAFETY: all pointers and lengths describe live local buffers.
        let status = unsafe {
            qrmi_select(
                selector.as_ptr(),
                request.as_ptr(),
                qpus.as_ptr(),
                qpus.len(),
                out.as_mut_ptr(),
                out.len(),
                msg.as_mut_ptr(),
                msg.len(),
            )
        };
        // SAFETY: the buffers are NUL-terminated by write_cstr or zeroed.
        let text = |buffer: &[c_char]| {
            unsafe { CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        (status, text(&out), text(&msg))
    }

    #[test]
    fn basic_picks_the_least_busy_with_enough_qubits() {
        let (a, b, c) = (owned("ibm_fez"), owned("ibm_marrakesh"), owned("ibm_miami"));
        let qpus = [
            qpu(&a, 156, 435, true),
            qpu(&b, 156, 3, true),
            qpu(&c, 120, 0, true),
        ];
        let (status, chosen, _) = call("basic", "qubits=128", &qpus);
        assert_eq!(status, QRMI_SELECT_OK);
        assert_eq!(chosen, "ibm_marrakesh");
    }

    #[test]
    fn unavailable_qpus_are_skipped() {
        let (a, b) = (owned("ibm_fez"), owned("ibm_marrakesh"));
        let qpus = [qpu(&a, 156, 435, true), qpu(&b, 156, 3, false)];
        let (_, chosen, _) = call("basic", "qubits=128", &qpus);
        assert_eq!(chosen, "ibm_fez");
    }

    #[test]
    fn nothing_matching_reports_reasons() {
        let a = owned("ibm_miami");
        let (status, chosen, message) = call("basic", "qubits=128", &[qpu(&a, 120, 0, true)]);
        assert_eq!(status, QRMI_SELECT_NONE);
        assert!(chosen.is_empty());
        assert!(message.contains("ibm_miami"), "{message}");
    }

    #[test]
    fn bad_selector_and_request_are_errors() {
        let a = owned("ibm_fez");
        let qpus = [qpu(&a, 156, 1, true)];
        assert_eq!(call("nonesuch", "qubits=1", &qpus).0, QRMI_SELECT_ERROR);
        assert_eq!(call("basic", "qubits=", &qpus).0, QRMI_SELECT_ERROR);
    }

    #[test]
    fn processor_type_round_trips_the_elim_spelling() {
        let heron = parse_processor_type("Heron_r2");
        assert_eq!(heron.family, "Heron");
        assert_eq!(heron.revision.as_deref(), Some("2"));
        assert_eq!(parse_processor_type("Nighthawk").revision, None);
        assert_eq!(parse_processor_type("odd_name").family, "odd_name");
    }

    #[test]
    fn processor_type_requests_match() {
        let (a, b) = (owned("ibm_fez"), owned("ibm_other"));
        let mut other = qpu(&b, 156, 0, true);
        let nighthawk = CString::new("Nighthawk").unwrap();
        other.processor_type = nighthawk.as_ptr();
        let qpus = [qpu(&a, 156, 50, true), other];
        let (_, chosen, _) = call("priority", "qubits=100;processor_type=Heron", &qpus);
        assert_eq!(chosen, "ibm_fez");
    }

    #[test]
    fn output_is_truncated_and_terminated() {
        let a = owned("ibm_a_very_long_backend_name");
        let qpus = [qpu(&a, 156, 1, true)];
        let selector = CString::new("basic").unwrap();
        let request = CString::new("qubits=1").unwrap();
        let mut out = [1 as c_char; 8];
        // SAFETY: live buffers; msg is null with zero length.
        let status = unsafe {
            qrmi_select(
                selector.as_ptr(),
                request.as_ptr(),
                qpus.as_ptr(),
                1,
                out.as_mut_ptr(),
                out.len(),
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(status, QRMI_SELECT_OK);
        // SAFETY: NUL-terminated by write_cstr.
        assert_eq!(unsafe { CStr::from_ptr(out.as_ptr()) }.to_bytes().len(), 7);
    }

    #[test]
    fn null_arguments_do_not_crash() {
        // SAFETY: null pointers are explicitly permitted for the strings'
        // failure path and for zero-length buffers.
        let status = unsafe {
            qrmi_select(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(status, QRMI_SELECT_ERROR);
    }
}
