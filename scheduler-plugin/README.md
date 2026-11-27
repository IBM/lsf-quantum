# schmod_qrmi: QPU selection in the LSF scheduler

An LSF external scheduler plug-in, written against the **public** `lssched.h`
API only. It moves QPU selection out of `jobstarter.qrmi` (where each job
queried QRMI at launch) and into the scheduler, using the load indices
`elim.qpu` already publishes.

## How a job flows

```
bsub -a "qrmi(file=.env,qpu.qubits=128,selector=priority)" app
  esub.qrmi   -> credentials into the job env      (as before)
              -> -extsched "QRMI[selector=priority|qpu=qubits=128]"   (new)
  mbschd / schmod_qrmi
     pre_proc   snapshot the QPU hosts' qubits, pending_jobs, ... 
     checkAlloc job untagged: run the policy (Rust, qpu-select), then
                extsched_modifyJob(rusage[...:qrmi_ibm_marrakesh=1]) and
                hold the job for this session
     next cycle job is tagged: released, dispatched normally
  jobstarter.qrmi
              -> bjobs -json -o effective_resreq  ->  qrmi_ibm_marrakesh
              -> acquires that QPU (no QRMI enumeration, no metrics fetch)
              -> bpost message as before
```

`extsched_modifyJob()` is the only public call that writes to a job, and it
writes only the resource requirement. That is why the assignment is a
resource usage.

## Prerequisites

1. **A shared numeric resource per QPU**, `qrmi_<qpu>`, in `lsf.shared`
   and `lsf.cluster.<name>`. Without it mbatchd rejects the tag and the job
   stays pending (the plug-in logs the rejection at `LOG_ERR`).
   Its value is the number of concurrent jobs allowed on that QPU:

   ```
   # lsf.shared, Resource section
   qrmi_ibm_fez   Numeric  ()  N  (concurrent jobs on ibm_fez)

   # lsf.cluster.<name>, ResourceMap section
   RESOURCENAME   LOCATION
   qrmi_ibm_fez   (8@[all])
   ```
   This is the standard static shared resource form; I have not tried it on
   a live cluster, so check it against your LSF version's documentation.

2. **`elim.qpu` as before**: the `ibm_*` Boolean per QPU host and the load
   indices (`qubits`, `pending_jobs`, ...).

3. **A host list** the plug-in reads each cycle: one host per line, `#`
   comments, names as LSF knows them (`lshosts`). Default
   `$LSF_ENVDIR/qrmi_qpu_hosts`, or point `QRMI_QPU_HOSTS` at another file
   in `mbschd`'s environment. QPU hosts need not be candidate hosts for jobs.

## Build and install

```
make LSF_INCDIR=/opt/lsf/10.1/include/lsf
make install LSF_LIBDIR=/opt/lsf/10.1/linux3.10-glibc2.17-x86_64/lib
```

Then add it **after** `schmod_default` in `lsb.modules` and run
`badmin mbdrestart`:

```
Begin PluginModule
SCH_PLUGIN        RB_PLUGIN   SCH_DISABLE_PHASES
schmod_default    ()          ()
schmod_qrmi       ()          ()
End PluginModule
```

`make test` runs a host-side harness that stubs the `extsched_*` calls and
exercises the real Rust policies; it needs only the header.

## Behaviour to know about

* **Selection happens at scheduling time**, from ELIM data (15 s old or
  more), not at launch. A job stays pending, with reason 20001, while no QPU
  satisfies its request.
* **A tagged job keeps its QPU.** It is not re-selected if queues shift.
* **`device=` jobs are tagged too**, without consulting metrics. The name
  must be a valid resource name and have a `qrmi_<name>` resource.
* **A QPU is "available" if its ELIM is reporting `qubits`.** There is no
  status index.
* **Rewriting the requirement.** The plug-in re-submits the job's
  `select[...]` and `rusage[...]` plus the tag. `order`, `span`, `same` and
  similar sections are not exposed by the public API; the job-level ones are
  lost if present. A job whose `rusage` uses phases or delays is left
  pending rather than rewritten (logged). Job arrays share one tag: the API
  modifies only the first element.
* **Fallback.** If `jobstarter.qrmi` finds no tag it selects locally, as
  before. `LSF_QRMI_SELECTION=scheduler` in the job environment makes that
  an error.
