# Examples
This directory provides an `example.py` QRMI-enabled application borrowed from ![Qiskit Runtime Service QRMI - Examples in Python](https://github.com/qiskit-community/spank-plugins/tree/main/qrmi/examples/python/qiskit_runtime_service) repository.

Shell script `run_example.sh` runs both `example.py ... sampler` and `example.py ... estimator`. The script assumes that QRMI Python package is installed as per ![Quantum Resource Management Interface(QRMI)](https://github.com/qiskit-community/spank-plugins/tree/main/qrmi) README.

`run_example.sh` can be executed under LSF with `esub.qrmi` as follows:

```
bsub -a "qrmi(.env, 128)" -o %J.out ./run_exampl.sh
Job <166> is submitted to default queue <normal>.
```

Upon completion you should see a bunch of ourtput files in CWD:
```
estimator_input_ibm_kingston.json
estimator_input_ibm_kingston_params_only.json
sampler_input_ibm_kingston.json
sampler_input_ibm_kingston_params_only.json
166.out
```
where `166.out` has stdout and stderr outputs from job <166>.


## Bell circuit through LSF and QRMI

`bell_test.py` submits a two-qubit Bell circuit with 128 shots using
QRMI SamplerV2 and the resource environment supplied by jobstarter.qrmi.
It retrieves measurement counts and checks that their total is 128.

See [the container quantum workflow](../Docker/README.md#running-a-quantum-job)
for building, mounting this directory, credentials, and submission.
This example submits a real quantum hardware job.
