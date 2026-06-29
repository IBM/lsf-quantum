# Distributed Hierarchical Quantum Circuit Cutting 
Scripts and data used for the IEEE QCE26 poster submission.

## POC source code
- `poster_mpi_4_hierarchical.py` - Two-level 4-way DHQCC implementation.
- `poster_mpi_8_hierarchical.py` - Two-level 4-way DHQCC implementation.
- `poster_sequential_baseline.py` - Baseline sequential CC. 
- `get_times.sh` - Extract circuit execution  times from the output.
- `sub_mpi.sh` and `sub_seq.sh` - experiments submission using IBM LSF.
- `Poster_Plots.ipynb` can be used to reproduce the _Circuits Execution Time_ figure.

## Experiments
You can run the experiments using the the following commands

- `./poster_sequential_baseline.py --num-samples 1000 --shots 4095 |tee out.base_1000`
- `mpiexec -n 8 --map-by core python poster_mpi_8_hierarchical.py --num-samples 100000 --shots 4096 |tee out.mpi8_10000`
- `mpiexec -n 4 --map-by core python poster_mpi_8_hierarchical.py --num-samples 100 --shots 4096 |tee out.mpi8_100`

Circuit execution times can be extracted by running `get_times.sh`.
