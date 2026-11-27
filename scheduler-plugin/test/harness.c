/* (C) Copyright 2025-2026 IBM. All Rights Reserved.
 *
 * This code is licensed under the Apache License, Version 2.0. You may
 * obtain a copy of this license in the LICENSE.txt file in the root directory
 * of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
 */

/* Host-side test of schmod_qrmi.c: the extsched_* API is stubbed, the
 * selection policies are the real Rust ones. */

#include <stdarg.h>
#include <assert.h>
#include <unistd.h>

#include "../schmod_qrmi.c"

static int failures = 0;
#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);    \
            failures++;                                                        \
        }                                                                      \
    } while (0)

/* ---- stub state -------------------------------------------------------- */

static struct hostResources hostTable[8];
static const char *hostNames[8];
static int nHosts = 0;

static extsched_rsrcReqInfo mockInfo;
static int mockHasInfo = 1;
static int mockJobId = 100, mockJobIndex = 0;
static int mockModError = 0;

static int modifyCalls = 0, holdCalls = 0;
static char lastResreq[2048];

void ls_syslog(int level, const char *fmt, ...) {
    (void)level; (void)fmt;
}
char *extsched_clustername(void) { return "cluster"; }
INT_Host *extsched_getHost(char *hostname, char *clustername) {
    int i;
    (void)clustername;
    for (i = 0; i < nHosts; i++) {
        if (strcmp(hostNames[i], hostname) == 0) {
            return (INT_Host *)&hostTable[i];
        }
    }
    return NULL;
}
struct hostResources *extsched_host_resources(INT_Host *host) {
    return (struct hostResources *)host;
}
int extsched_getJobInfo(INT_Job *job, struct jobInfo *info) {
    (void)job;
    info->jobId = mockJobId;
    info->jobIndex = mockJobIndex;
    return 0;
}
void extsched_freeJobInfo(struct jobInfo *info) { (void)info; }
INT_RsrcReq *extsched_getRsrcReqForJob(INT_Job *job) {
    (void)job;
    return mockHasInfo ? (INT_RsrcReq *)&mockInfo : NULL;
}
extsched_rsrcReqInfo *extsched_getRsrcReqInfo(INT_RsrcReq *r) {
    return (extsched_rsrcReqInfo *)r;
}
void extsched_freeRsrcReqInfo(extsched_rsrcReqInfo **i) { (void)i; }
int extsched_modifyJob(INT_Job *job, char *rsrcReq) {
    (void)job;
    modifyCalls++;
    snprintf(lastResreq, sizeof(lastResreq), "%s", rsrcReq);
    return 0;
}
int extsched_getSchedJobModError(INT_Job *job, int *nElem,
                                 extsched_JobElement **jobList, int **errList) {
    (void)job;
    if (!mockModError) {
        return -1;
    }
    *nElem = 1;
    *jobList = calloc(1, sizeof(**jobList));
    (*jobList)[0].baseId = mockJobId;
    (*jobList)[0].index = mockJobIndex;
    *errList = calloc(1, sizeof(int));
    (*errList)[0] = mockModError;
    return 0;
}
int extsched_getAlloc(INT_Alloc *alloc, extsched_Alloc **out) {
    extsched_Alloc *a = calloc(1, sizeof(*a));
    (void)alloc;
    a->nInst = 1;
    a->inst = calloc(1, sizeof(extsched_AllocInst *));
    a->inst[0] = calloc(1, sizeof(extsched_AllocInst));
    a->inst[0]->host = strdup("node1");
    a->inst[0]->rsrc = strdup(slot_);
    a->inst[0]->amount = 1;
    *out = a;
    return 0;
}
void extsched_freeAlloc(extsched_Alloc **a) {
    free((*a)->inst[0]->host);
    free((*a)->inst[0]->rsrc);
    free((*a)->inst[0]);
    free((*a)->inst);
    free(*a);
    *a = NULL;
}
int extsched_addAllocLimit(INT_AllocLimitList *l, INT_Alloc *a, char *host,
                           int max, int reason, char *detail, int handler,
                           int bucket) {
    (void)l; (void)a; (void)host; (void)max; (void)reason; (void)detail;
    (void)handler; (void)bucket;
    holdCalls++;
    return 0;
}
int extsched_determineEvent(INT_Job *j, INT_Alloc *a, INT_AllocLimitList *l,
                            int f) {
    (void)j; (void)a; (void)l; (void)f;
    return SCH_FM_EVE_DEALLOCATE;
}
char **extsched_resreq_getextresreq(INT_RsrcReq *r, int *n) {
    (void)r; (void)n;
    return NULL;
}
void extsched_resreq_setobject(INT_RsrcReq *r, int id, char *key, void *obj) {
    (void)r; (void)id; (void)key; (void)obj;
}
int extsched_resreq_registerhandler(int id, RsrcReqHandlerType *h) {
    (void)id; (void)h;
    return 0;
}

/* ---- helpers ----------------------------------------------------------- */

static void add_host(const char *host, const char *qpu, float qubits,
                     float pending, const char *processor) {
    static struct resources res[8][8];
    struct hostResources *hr = &hostTable[nHosts];
    struct resources *r = res[nHosts];
    int n = 0;

    memset(r, 0, sizeof(res[0]));
    if (qpu) {
        r[n].resName = (char *)qpu; r[n].cType = BOOL_VAL; r[n].val.bval = 1; n++;
    }
    r[n].resName = "qubits"; r[n].cType = FLOAT_VAL; r[n].val.fval = qubits; n++;
    r[n].resName = "pending_jobs"; r[n].cType = FLOAT_VAL; r[n].val.fval = pending; n++;
    r[n].resName = "processor_type"; r[n].cType = STRING_VAL;
    r[n].val.sval = (char *)processor; n++;

    hr->hostName = (char *)host;
    hr->nres = n;
    hr->res = r;
    hostNames[nHosts++] = host;
}

static void reset_job(const char *select) {
    static char selectBuf[256];
    static extsched_rsrcConsump consump[4];

    memset(&mockInfo, 0, sizeof(mockInfo));
    memset(consump, 0, sizeof(consump));
    snprintf(selectBuf, sizeof(selectBuf), "%s", select);
    mockInfo.selectStr = selectBuf;
    mockInfo.rsrcConsump = consump;
    mockHasInfo = 1;
    mockModError = 0;
    modifyCalls = holdCalls = 0;
    lastResreq[0] = '\0';
    while (pendingTags) {
        pending_remove(pendingTags->jobId, pendingTags->jobIndex);
    }
}

static void add_usage(const char *name, float amount) {
    extsched_rsrcConsump *c = &mockInfo.rsrcConsump[mockInfo.nRsrcConsump++];
    c->rsrcName = (char *)name;
    c->amount = amount;
}

static void run_check(const char *text) {
    qrmi_request *req = request_parse(text);
    qrmi_checkAlloc(req, (void *)1, (void *)2, (void *)3);
    qrmi_free(req);
}

/* ---- tests ------------------------------------------------------------- */

static void test_request_parsing(void) {
    qrmi_request *r = request_parse(
        "foo;QRMI[selector=priority|device=|qpu=qubits=128;pending_jobs=100]");
    CHECK(r && strcmp(r->selector, "priority") == 0);
    CHECK(r && r->device == NULL);
    CHECK(r && strcmp(r->request, "qubits=128;pending_jobs=100") == 0);
    qrmi_free(r);

    r = request_parse("QRMI[device=ibm_fez]");
    CHECK(r && strcmp(r->selector, "basic") == 0); /* defaulted */
    CHECK(r && strcmp(r->device, "ibm_fez") == 0);
    qrmi_free(r);

    CHECK(request_parse("OTHER[x=1]") == NULL);
    CHECK(request_parse("QRMI[unterminated") == NULL);
}

static void test_snapshot(void) {
    char path[] = "/tmp/qrmi_hosts_XXXXXX";
    int fd = mkstemp(path);
    FILE *fp = fdopen(fd, "w");

    add_host("qhost1", "ibm_fez", 156, 435, "Heron_r2");
    add_host("qhost2", "ibm_marrakesh", 156, 3, "Heron_r2");
    add_host("qhost3", "ibm_miami", 120, 0, "Nighthawk");
    add_host("plain", NULL, 16, 0, "-");
    add_host("silent", "ibm_torino", 2.147e9f, 2.147e9f, "-"); /* ELIM silent */

    fprintf(fp, "# QPU hosts\nqhost1\n  qhost2  \nqhost3\nplain\nsilent\nnosuch\n");
    fclose(fp);
    setenv("QRMI_QPU_HOSTS", path, 1);

    snapshot_refresh();
    unlink(path);

    CHECK(snapshotCount == 4); /* plain has no ibm_ resource; nosuch is unknown */
    CHECK(strcmp(snapshot[0].name, "ibm_fez") == 0);
    CHECK(snapshot[0].qpu.qubits == 156 && snapshot[0].qpu.pending_jobs == 435);
    CHECK(snapshot[0].qpu.available);
    CHECK(strcmp(snapshot[0].qpu.processor_type, "Heron_r2") == 0);
    CHECK(strcmp(snapshot[3].name, "ibm_torino") == 0);
    CHECK(snapshot[3].qpu.qubits == -1 && !snapshot[3].qpu.available);
}

static void test_selection_tags_the_job(void) {
    reset_job("type==any");
    add_usage("mem", 100);
    run_check("QRMI[selector=basic|qpu=qubits=128]");

    CHECK(modifyCalls == 1);
    CHECK(strcmp(lastResreq,
                 "select[type==any] rusage[mem=100:qrmi_ibm_marrakesh=1]") == 0);
    CHECK(holdCalls == 1); /* held until mbatchd applies the tag */
    CHECK(pending_find(mockJobId, mockJobIndex) != NULL);
}

static void test_waiting_job_is_not_re_requested(void) {
    reset_job("");
    run_check("QRMI[qpu=qubits=128]");
    CHECK(modifyCalls == 1);
    CHECK(strcmp(lastResreq, "rusage[qrmi_ibm_marrakesh=1]") == 0);

    run_check("QRMI[qpu=qubits=128]"); /* next session, tag not applied yet */
    CHECK(modifyCalls == 1);
    CHECK(holdCalls == 2);
}

static void test_tagged_job_is_released(void) {
    reset_job("");
    run_check("QRMI[qpu=qubits=128]");
    add_usage("qrmi_ibm_marrakesh", 1); /* mbatchd applied it */
    holdCalls = modifyCalls = 0;

    run_check("QRMI[qpu=qubits=128]");
    CHECK(modifyCalls == 0 && holdCalls == 0);
    CHECK(pending_find(mockJobId, mockJobIndex) == NULL);
}

static void test_rejected_modification_is_retried(void) {
    reset_job("");
    run_check("QRMI[qpu=qubits=128]");
    CHECK(modifyCalls == 1);

    mockModError = 7;
    run_check("QRMI[qpu=qubits=128]");
    CHECK(modifyCalls == 2);
}

static void test_explicit_device(void) {
    reset_job("");
    run_check("QRMI[device=ibm_sherbrooke]"); /* no metrics consulted */
    CHECK(strcmp(lastResreq, "rusage[qrmi_ibm_sherbrooke=1]") == 0);

    reset_job("");
    run_check("QRMI[device=bad name;rm]");
    CHECK(modifyCalls == 0 && holdCalls == 1);
}

static void test_nothing_matches_holds_the_job(void) {
    reset_job("");
    run_check("QRMI[qpu=qubits=500]");
    CHECK(modifyCalls == 0 && holdCalls == 1);
}

static void test_unreproducible_rusage_is_not_rewritten(void) {
    reset_job("");
    add_usage("mem", 100);
    mockInfo.rsrcConsump[0].numPhases = 2;
    run_check("QRMI[qpu=qubits=128]");
    CHECK(modifyCalls == 0 && holdCalls == 1);
}

static void test_not_a_quantum_job(void) {
    reset_job("");
    qrmi_checkAlloc(NULL, (void *)1, (void *)2, (void *)3);
    CHECK(modifyCalls == 0 && holdCalls == 0);
}

int main(void) {
    test_request_parsing();
    test_snapshot();
    test_selection_tags_the_job();
    test_waiting_job_is_not_re_requested();
    test_tagged_job_is_released();
    test_rejected_modification_is_retried();
    test_explicit_device();
    test_nothing_matches_holds_the_job();
    test_unreproducible_rusage_is_not_rewritten();
    test_not_a_quantum_job();

    if (failures) {
        fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    printf("schmod_qrmi harness: all checks passed\n");
    return 0;
}
