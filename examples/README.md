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
