/* (C) Copyright 2025-2026 IBM. All Rights Reserved.
 *
 * This code is licensed under the Apache License, Version 2.0. You may
 * obtain a copy of this license in the LICENSE.txt file in the root directory
 * of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
 */
#ifndef QRMI_SELECT_H
#define QRMI_SELECT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* One candidate QPU. Absent metrics: negative integer, NaN double, NULL
 * string. Strings are borrowed for the duration of the call. */
typedef struct qrmi_qpu {
    const char *name;
    int64_t qubits;
    int64_t pending_jobs;
    double clops;
    double readout_error_median;
    double sx_error_median;
    double cz_error_median;
    double t1_median_us;
    double t2_median_us;
    const char *qpu_version;
    const char *processor_type; /* "Heron_r2" or "Heron" */
    int available;              /* non-zero: accepting work */
} qrmi_qpu;

#define QRMI_SELECT_OK 0        /* out holds the chosen QPU name */
#define QRMI_SELECT_NONE 1      /* nothing satisfies the request; msg says why */
#define QRMI_SELECT_ERROR (-1)  /* bad selector/request/arguments; msg says why */

/* Choose a QPU. `selector` is basic|health|priority; `request` is the
 * ';'-separated attribute string the esub produces (e.g.
 * "qubits=128;pending_jobs=100"). `out` and `msg` are NUL-terminated and
 * truncated to their lengths. Never panics across the boundary. */
int qrmi_select(const char *selector, const char *request,
                const qrmi_qpu *qpus, size_t n_qpus, char *out,
                size_t out_len, char *msg, size_t msg_len);

#ifdef __cplusplus
}
#endif
#endif /* QRMI_SELECT_H */
