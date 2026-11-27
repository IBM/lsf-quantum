/* (C) Copyright 2025-2026 IBM. All Rights Reserved.
 *
 * This code is licensed under the Apache License, Version 2.0. You may
 * obtain a copy of this license in the LICENSE.txt file in the root directory
 * of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
 */

/*
 * schmod_qrmi -- LSF external scheduler plug-in that selects the QPU for a
 * quantum job and records the choice on the job.
 *
 * Built only on the public extsched_* API of lssched.h.
 *
 * Flow, per pending job that carries a QRMI[...] -extsched request:
 *
 *   1. sched_pre_proc  snapshots the QPU hosts' load indices (the ELIM's
 *                      qubits, pending_jobs, ...) into a private table.
 *   2. checkAlloc      if the job is not yet tagged, runs the selection
 *                      policy (Rust, via qrmi_select()) over the snapshot,
 *                      then asks mbatchd -- with extsched_modifyJob() -- to
 *                      add   rusage[qrmi_<qpu>=1]   to the job's resource
 *                      requirement, and holds the job for this session.
 *   3. next session    the tag is on the job; the job is released and is
 *                      dispatched normally. jobstarter.qrmi reads the tag
 *                      back with  bjobs -o effective_resreq.
 *
 * extsched_modifyJob() is the only public call that writes to a job, and it
 * only writes the resource requirement. Hence the tag is a resource.
 *
 * Operator prerequisites (see README.md in this directory):
 *   - a shared numeric resource  qrmi_<qpu>  per QPU in lsf.shared and
 *     lsf.cluster.<name> (it also caps concurrent jobs per QPU);
 *   - the file named by $QRMI_QPU_HOSTS (default $LSF_ENVDIR/qrmi_qpu_hosts)
 *     listing the hosts that run elim.qpu, one per line.
 */

#include <ctype.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "lssched.h"
#include "qrmi_select.h"

static const int HANDLER_ID = 113; /* 100-199; 111 and 112 are the examples */

#define PEND_QRMI_REASON 20001 /* customer pending reason, 20001-25000 */
#define TAG_PREFIX "qrmi_"
#define QPU_PREFIX "ibm_"       /* the ELIM's Boolean per-QPU resource */
#define MAX_QPUS 64
#define NAME_LEN 64
#define MODIFY_RETRY_SECS 60    /* re-request a tag mbatchd never applied */
#define ENTRY_MAX_AGE_SECS 3600 /* forget jobs that left without a tag */
#define MISSING_LOAD 1.0e9f     /* LSF's "no value" load is ~2.1e9 */

#ifndef FREEUP
#define FREEUP(p)                                                              \
    {                                                                          \
        if (p) {                                                               \
            free(p);                                                           \
            (p) = NULL;                                                        \
        }                                                                      \
    }
#endif

/* ---- the user's request, parsed from -extsched "QRMI[...]" ------------ */

typedef struct {
    char *selector; /* basic | health | priority; never NULL */
    char *device;   /* explicit device, or NULL */
    char *request;  /* ';'-separated attributes, or "" */
} qrmi_request;

/* ---- jobs we have asked mbatchd to tag --------------------------------- */

typedef struct pending_tag {
    struct pending_tag *next;
    int jobId;
    int jobIndex;
    time_t requested;
} pending_tag;

static pending_tag *pendingTags = NULL;

/* ---- the QPU snapshot, rebuilt every session --------------------------- */

typedef struct {
    char name[NAME_LEN];
    char version[NAME_LEN];
    char processor[NAME_LEN];
    qrmi_qpu qpu; /* string members point into this entry */
} snapshot_entry;

static snapshot_entry snapshot[MAX_QPUS];
static int snapshotCount = 0;

/* ---- forward declarations ---------------------------------------------- */

static int qrmi_new(void *resreq);
static void qrmi_free(void *data);
static int qrmi_match(void *data, void *candGroupList, void *reasonTb);
static int qrmi_sort(void *data, void *candGroupList, void *reasonTb);
static int qrmi_checkAlloc(void *data, void *job, void *alloc,
                           void *allocLimitList);
static int qrmi_notifyAlloc(void *data, void *job, void *alloc,
                            void *allocLimitList, int flag);

/* ======================================================================== */
/* pending-tag list                                                         */
/* ======================================================================== */

static pending_tag *pending_find(int jobId, int jobIndex) {
    pending_tag *p;
    for (p = pendingTags; p; p = p->next) {
        if (p->jobId == jobId && p->jobIndex == jobIndex) {
            return p;
        }
    }
    return NULL;
}

static void pending_add(int jobId, int jobIndex) {
    pending_tag *p = pending_find(jobId, jobIndex);

    if (p == NULL) {
        p = (pending_tag *)calloc(1, sizeof(*p));
        if (p == NULL) {
            return;
        }
        p->jobId = jobId;
        p->jobIndex = jobIndex;
        p->next = pendingTags;
        pendingTags = p;
    }
    p->requested = time(NULL);
}

static void pending_remove(int jobId, int jobIndex) {
    pending_tag **link;

    for (link = &pendingTags; *link; link = &(*link)->next) {
        if ((*link)->jobId == jobId && (*link)->jobIndex == jobIndex) {
            pending_tag *gone = *link;
            *link = gone->next;
            free(gone);
            return;
        }
    }
}

/* Drop entries for jobs that were killed before they were ever tagged. */
static void pending_purge(void) {
    time_t now = time(NULL);
    pending_tag **link = &pendingTags;

    while (*link) {
        if (now - (*link)->requested > ENTRY_MAX_AGE_SECS) {
            pending_tag *gone = *link;
            *link = gone->next;
            free(gone);
        } else {
            link = &(*link)->next;
        }
    }
}

/* ======================================================================== */
/* request parsing                                                          */
/* ======================================================================== */

static char *dup_range(const char *start, size_t length) {
    char *copy = (char *)malloc(length + 1);
    if (copy) {
        memcpy(copy, start, length);
        copy[length] = '\0';
    }
    return copy;
}

/* QRMI[selector=priority|device=ibm_fez|qpu=qubits=128;pending_jobs=100]
 * Segments are separated by '|', each is key=value split at the FIRST '='
 * (so the qpu value may itself contain '='). */
static qrmi_request *request_parse(const char *text) {
    const char *open = strstr(text, "QRMI[");
    const char *close;
    qrmi_request *req;
    char *body, *segment, *save = NULL;

    if (open == NULL) {
        return NULL;
    }
    open += strlen("QRMI[");
    close = strchr(open, ']');
    if (close == NULL) {
        return NULL;
    }

    req = (qrmi_request *)calloc(1, sizeof(*req));
    body = dup_range(open, (size_t)(close - open));
    if (req == NULL || body == NULL) {
        free(req);
        free(body);
        return NULL;
    }

    for (segment = strtok_r(body, "|", &save); segment;
         segment = strtok_r(NULL, "|", &save)) {
        char *eq = strchr(segment, '=');
        char *value;

        if (eq == NULL) {
            continue;
        }
        *eq = '\0';
        value = eq + 1;

        if (strcmp(segment, "selector") == 0) {
            FREEUP(req->selector);
            req->selector = strdup(value);
        } else if (strcmp(segment, "device") == 0) {
            FREEUP(req->device);
            if (*value) {
                req->device = strdup(value);
            }
        } else if (strcmp(segment, "qpu") == 0) {
            FREEUP(req->request);
            req->request = strdup(value);
        }
    }
    free(body);

    if (req->selector == NULL || *req->selector == '\0') {
        FREEUP(req->selector);
        req->selector = strdup("basic");
    }
    if (req->request == NULL) {
        req->request = strdup("");
    }
    if (req->selector == NULL || req->request == NULL) {
        qrmi_free(req);
        return NULL;
    }
    return req;
}

/* A device name becomes part of a resource name, so it must be one. */
static int valid_resource_chars(const char *name) {
    size_t i;

    if (name == NULL || *name == '\0' || strlen(name) > NAME_LEN - 1) {
        return 0;
    }
    for (i = 0; name[i]; i++) {
        if (!isalnum((unsigned char)name[i]) && name[i] != '_') {
            return 0;
        }
    }
    return 1;
}

/* ======================================================================== */
/* the QPU snapshot                                                         */
/* ======================================================================== */

static double load_value(const struct resources *res) {
    if (res->cType != FLOAT_VAL || res->val.fval != res->val.fval
        || res->val.fval < 0.0f || res->val.fval >= MISSING_LOAD) {
        return NAN;
    }
    return (double)res->val.fval;
}

static void entry_reset(snapshot_entry *e) {
    memset(e, 0, sizeof(*e));
    e->qpu.qubits = -1;
    e->qpu.pending_jobs = -1;
    e->qpu.clops = e->qpu.readout_error_median = e->qpu.sx_error_median =
        e->qpu.cz_error_median = e->qpu.t1_median_us = e->qpu.t2_median_us =
            NAN;
}

/* Fill one snapshot entry from a host's resources. Returns 1 if the host
 * serves a QPU. */
static int entry_from_host(snapshot_entry *e, struct hostResources *hr) {
    int k, isQpu = 0;

    entry_reset(e);

    for (k = 0; k < hr->nres; k++) {
        const struct resources *r = &hr->res[k];
        const char *n = r->resName;

        if (n == NULL) {
            continue;
        }
        if (r->cType == BOOL_VAL) {
            if (r->val.bval != 0 && strncmp(n, QPU_PREFIX, strlen(QPU_PREFIX)) == 0
                && strlen(n) > strlen(QPU_PREFIX) && !isQpu) {
                snprintf(e->name, sizeof(e->name), "%s", n);
                isQpu = 1;
            }
        } else if (r->cType == FLOAT_VAL) {
            double v = load_value(r);

            if (strcmp(n, "qubits") == 0 && v == v) {
                e->qpu.qubits = (int64_t)v;
            } else if (strcmp(n, "pending_jobs") == 0 && v == v) {
                e->qpu.pending_jobs = (int64_t)v;
            } else if (strcmp(n, "clops") == 0) {
                e->qpu.clops = v;
            } else if (strcmp(n, "readout_error_median") == 0) {
                e->qpu.readout_error_median = v;
            } else if (strcmp(n, "sx_error_median") == 0) {
                e->qpu.sx_error_median = v;
            } else if (strcmp(n, "cz_error_median") == 0) {
                e->qpu.cz_error_median = v;
            } else if (strcmp(n, "T1_median_\xC2\xB5s") == 0) { /* T1_median_µs */
                e->qpu.t1_median_us = v;
            } else if (strcmp(n, "T2_median_\xC2\xB5s") == 0) {
                e->qpu.t2_median_us = v;
            }
        } else if (r->cType == STRING_VAL && r->val.sval != NULL
                   && strcmp(r->val.sval, "-") != 0) {
            if (strcmp(n, "qpu_version") == 0) {
                snprintf(e->version, sizeof(e->version), "%s", r->val.sval);
            } else if (strcmp(n, "processor_type") == 0) {
                snprintf(e->processor, sizeof(e->processor), "%s", r->val.sval);
            }
        }
    }

    if (!isQpu) {
        return 0;
    }

    e->qpu.name = e->name;
    e->qpu.qpu_version = e->version[0] ? e->version : NULL;
    e->qpu.processor_type = e->processor[0] ? e->processor : NULL;
    /* The ELIM publishes no status index: a QPU whose ELIM is not
     * reporting has no qubit count, and is treated as unavailable. */
    e->qpu.available = e->qpu.qubits >= 0;
    return 1;
}

static void snapshot_refresh(void) {
    static char fname[] = "qrmi_snapshot_refresh";
    const char *path = getenv("QRMI_QPU_HOSTS");
    char fallback[1024];
    char line[512];
    FILE *fp;

    snapshotCount = 0;

    if (path == NULL) {
        const char *envdir = getenv("LSF_ENVDIR");
        if (envdir == NULL) {
            ls_syslog(LOG_ERR, "%s: neither QRMI_QPU_HOSTS nor LSF_ENVDIR set",
                      fname);
            return;
        }
        snprintf(fallback, sizeof(fallback), "%s/qrmi_qpu_hosts", envdir);
        path = fallback;
    }

    fp = fopen(path, "r");
    if (fp == NULL) {
        ls_syslog(LOG_ERR, "%s: cannot open %s", fname, path);
        return;
    }

    while (snapshotCount < MAX_QPUS && fgets(line, sizeof(line), fp)) {
        char *host = line, *end;
        INT_Host *ih;
        struct hostResources *hr;

        while (isspace((unsigned char)*host)) {
            host++;
        }
        end = host + strlen(host);
        while (end > host && isspace((unsigned char)end[-1])) {
            *--end = '\0';
        }
        if (*host == '\0' || *host == '#') {
            continue;
        }

        ih = extsched_getHost(host, extsched_clustername());
        if (ih == NULL) {
            ls_syslog(LOG_DEBUG, "%s: host <%s> unknown to the scheduler",
                      fname, host);
            continue;
        }
        /* Static memory, valid until the next call: copy out at once. */
        hr = extsched_host_resources(ih);
        if (hr == NULL) {
            continue;
        }
        if (entry_from_host(&snapshot[snapshotCount], hr)) {
            snapshotCount++;
        }
    }
    fclose(fp);

    ls_syslog(LOG_DEBUG, "%s: %d QPU(s) in snapshot", fname, snapshotCount);
}

/* ======================================================================== */
/* selection                                                                */
/* ======================================================================== */

/* Returns 0 and fills `chosen`, or -1 with `why` set. */
static int choose_qpu(const qrmi_request *req, char *chosen, size_t chosenLen,
                      char *why, size_t whyLen) {
    qrmi_qpu table[MAX_QPUS];
    int i, status;

    if (req->device != NULL) {
        if (!valid_resource_chars(req->device)) {
            snprintf(why, whyLen, "device <%s> is not a valid resource name",
                     req->device);
            return -1;
        }
        snprintf(chosen, chosenLen, "%s", req->device);
        return 0;
    }

    for (i = 0; i < snapshotCount; i++) {
        table[i] = snapshot[i].qpu;
    }

    status = qrmi_select(req->selector, req->request, table,
                         (size_t)snapshotCount, chosen, chosenLen, why, whyLen);
    if (status == QRMI_SELECT_OK) {
        return 0;
    }
    if (why[0] == '\0') {
        snprintf(why, whyLen, "no QPU satisfies the request");
    }
    return -1;
}

/* ======================================================================== */
/* the job's resource requirement                                           */
/* ======================================================================== */

/* If the job already carries a qrmi_<qpu> usage, copy <qpu> and return 1. */
static int job_tag(extsched_rsrcReqInfo *info, char *qpu, size_t qpuLen) {
    int k;

    for (k = 0; k < info->nRsrcConsump; k++) {
        const char *n = info->rsrcConsump[k].rsrcName;

        if (n && strncmp(n, TAG_PREFIX, strlen(TAG_PREFIX)) == 0
            && n[strlen(TAG_PREFIX)] != '\0') {
            snprintf(qpu, qpuLen, "%s", n + strlen(TAG_PREFIX));
            return 1;
        }
    }
    return 0;
}

/* select[<sel>] rusage[<existing>:qrmi_<qpu>=1]. Refuses (returns -1) when
 * the existing rusage uses phases or delays, which cannot be reproduced from
 * what the public API exposes. */
static int build_resreq(const extsched_rsrcReqInfo *info, const char *qpu,
                        char *out, size_t outLen) {
    size_t used = 0;
    int k;

    out[0] = '\0';

    if (info->selectStr && *info->selectStr) {
        used += (size_t)snprintf(out + used, outLen - used, "select[%s] ",
                                 info->selectStr);
        if (used >= outLen) {
            return -1;
        }
    }

    used += (size_t)snprintf(out + used, outLen - used, "rusage[");
    for (k = 0; k < info->nRsrcConsump && used < outLen; k++) {
        const extsched_rsrcConsump *c = &info->rsrcConsump[k];

        if (c->numPhases > 0 || c->delay != 0 || c->rate != 0.0f) {
            return -1;
        }
        used += (size_t)snprintf(out + used, outLen - used, "%s=%g:",
                                 c->rsrcName, (double)c->amount);
    }
    if (used >= outLen) {
        return -1;
    }
    used += (size_t)snprintf(out + used, outLen - used, "%s%s=1]", TAG_PREFIX,
                             qpu);
    return used < outLen ? 0 : -1;
}

/* Keep the job pending in this session. */
static void hold_job(void *alloc, void *allocLimitList) {
    extsched_Alloc *a = NULL;
    int k;

    if (alloc == NULL || extsched_getAlloc(alloc, &a) < 0) {
        return;
    }
    for (k = 0; k < a->nInst; k++) {
        if (a->inst[k]->rsrc && strcmp(a->inst[k]->rsrc, slot_) == 0) {
            extsched_addAllocLimit(allocLimitList, alloc, a->inst[k]->host, 0,
                                   PEND_QRMI_REASON, "qrmi", HANDLER_ID, 0);
        }
    }
    extsched_freeAlloc(&a);
}

/* Did mbatchd refuse the modification we asked for last session? */
static int modify_was_rejected(void *job, int jobId, int jobIndex) {
    extsched_JobElement *elements = NULL;
    int *errors = NULL;
    int n = 0, k, rejected = 0;

    if (extsched_getSchedJobModError(job, &n, &elements, &errors) != 0) {
        return 0;
    }
    for (k = 0; k < n; k++) {
        if (errors[k] != 0 && elements[k].baseId == jobId
            && elements[k].index == jobIndex) {
            rejected = errors[k];
        }
        FREEUP(elements[k].clusterId);
    }
    FREEUP(elements);
    FREEUP(errors);
    return rejected;
}

/* ======================================================================== */
/* handler callbacks                                                        */
/* ======================================================================== */

static int qrmi_new(void *resreq) {
    char **ext;
    int num = 0;
    qrmi_request *req;
    char key[1024];

    if (resreq == NULL) {
        return 0;
    }
    ext = extsched_resreq_getextresreq(resreq, &num);
    if (ext == NULL || num <= 1 || ext[1] == NULL) {
        return 0;
    }
    req = request_parse(ext[1]);
    if (req == NULL) {
        return 0;
    }
    snprintf(key, sizeof(key), "#%s#", ext[1]);
    extsched_resreq_setobject(resreq, HANDLER_ID, key, req);
    return 0;
}

static void qrmi_free(void *data) {
    qrmi_request *req = (qrmi_request *)data;

    if (req) {
        FREEUP(req->selector);
        FREEUP(req->device);
        FREEUP(req->request);
        free(req);
    }
}

/* The QPU is not a host property of the job's execution host, so the plug-in
 * does not filter or reorder candidate hosts. */
static int qrmi_match(void *data, void *candGroupList, void *reasonTb) {
    (void)data; (void)candGroupList; (void)reasonTb;
    return 0;
}

static int qrmi_sort(void *data, void *candGroupList, void *reasonTb) {
    (void)data; (void)candGroupList; (void)reasonTb;
    return 0;
}

static int qrmi_checkAlloc(void *data, void *job, void *alloc,
                           void *allocLimitList) {
    static char fname[] = "qrmi_checkAlloc";
    qrmi_request *req = (qrmi_request *)data;
    struct jobInfo info = {0};
    INT_RsrcReq *rsrcreq;
    extsched_rsrcReqInfo *rinfo;
    char chosen[NAME_LEN], why[256], resreq[2048], tagged[NAME_LEN];
    pending_tag *waiting;
    int rejected;

    if (req == NULL || job == NULL) {
        return 0; /* not a quantum job */
    }
    if (extsched_getJobInfo(job, &info) < 0) {
        return 0;
    }

    rsrcreq = extsched_getRsrcReqForJob(job);
    rinfo = rsrcreq ? extsched_getRsrcReqInfo(rsrcreq) : NULL;
    if (rinfo == NULL) {
        ls_syslog(LOG_DEBUG, "%s: job %d[%d]: no resource requirement info",
                  fname, info.jobId, info.jobIndex);
        goto hold;
    }

    /* Tagged: release the job. */
    if (job_tag(rinfo, tagged, sizeof(tagged))) {
        ls_syslog(LOG_DEBUG, "%s: job %d[%d] tagged for QPU <%s>", fname,
                  info.jobId, info.jobIndex, tagged);
        pending_remove(info.jobId, info.jobIndex);
        goto release;
    }

    /* Asked last session: wait for mbatchd, unless it refused or forgot. */
    waiting = pending_find(info.jobId, info.jobIndex);
    rejected = waiting ? modify_was_rejected(job, info.jobId, info.jobIndex) : 0;
    if (rejected) {
        ls_syslog(LOG_ERR,
                  "%s: mbatchd rejected the QPU tag for job %d[%d] (error %d);"
                  " is a qrmi_<qpu> resource defined for it in lsf.shared?",
                  fname, info.jobId, info.jobIndex, rejected);
    }
    if (waiting && !rejected
        && time(NULL) - waiting->requested < MODIFY_RETRY_SECS) {
        goto hold;
    }

    if (choose_qpu(req, chosen, sizeof(chosen), why, sizeof(why)) < 0) {
        ls_syslog(LOG_DEBUG, "%s: job %d[%d] not placed: %s", fname,
                  info.jobId, info.jobIndex, why);
        goto hold;
    }
    if (build_resreq(rinfo, chosen, resreq, sizeof(resreq)) < 0) {
        ls_syslog(LOG_ERR,
                  "%s: job %d[%d]: cannot add the QPU tag to its rusage "
                  "(phased/delayed usage or too long)", fname, info.jobId,
                  info.jobIndex);
        goto hold;
    }

    ls_syslog(LOG_INFO, "%s: job %d[%d] -> QPU <%s>: %s", fname, info.jobId,
              info.jobIndex, chosen, resreq);
    if (extsched_modifyJob(job, resreq) == 0) {
        pending_add(info.jobId, info.jobIndex);
    }

hold:
    hold_job(alloc, allocLimitList);
release:
    extsched_freeRsrcReqInfo(&rinfo);
    extsched_freeJobInfo(&info);
    return 0;
}

static int qrmi_notifyAlloc(void *data, void *job, void *alloc,
                            void *allocLimitList, int flag) {
    (void)data;

    if (job) {
        struct jobInfo info = {0};

        if (extsched_getJobInfo(job, &info) == 0) {
            if (extsched_determineEvent(job, alloc, allocLimitList, flag)
                == SCH_FM_EVE_DEALLOCATE) {
                pending_remove(info.jobId, info.jobIndex);
            }
            extsched_freeJobInfo(&info);
        }
    }
    return 0;
}

/* ======================================================================== */
/* plug-in entry points                                                     */
/* ======================================================================== */

int sched_version(void *param) {
    (void)param;
    return 0;
}

int sched_init(void *param) {
    static char fname[] = "qrmi_sched_init";
    RsrcReqHandlerType *handler;

    (void)param;

    handler = (RsrcReqHandlerType *)calloc(1, sizeof(RsrcReqHandlerType));
    if (handler == NULL) {
        ls_syslog(LOG_ERR, "%s: calloc() failed", fname);
        return -1;
    }
    handler->newFn = (RsrcReqHandler_NewFn)qrmi_new;
    handler->freeFn = (RsrcReqHandler_FreeFn)qrmi_free;
    handler->matchFn = (RsrcReqHandler_MatchFn)qrmi_match;
    handler->sortFn = (RsrcReqHandler_SortFn)qrmi_sort;
    handler->checkAllocFn = (RsrcReqHandler_CheckAllocFn)qrmi_checkAlloc;
    handler->notifyAllocFn = (RsrcReqHandler_NotifyAllocFn)qrmi_notifyAlloc;

    /* The header says the handler's memory must not be freed. */
    extsched_resreq_registerhandler(HANDLER_ID, handler);

    ls_syslog(LOG_INFO, "%s: schmod_qrmi loaded", fname);
    return 0;
}

int sched_pre_proc(void *param) {
    (void)param;
    snapshot_refresh();
    pending_purge();
    return 0;
}

int sched_match_limit(void *param) { (void)param; return 0; }
int sched_order_alloc(void *param) { (void)param; return 0; }
int sched_post_proc(void *param) { (void)param; return 0; }
int sched_finalize(void *param) { (void)param; return 0; }
