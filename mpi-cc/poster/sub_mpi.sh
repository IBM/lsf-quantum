#!/bin/bash

# (C) Copyright 2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
#
# Any modifications or derivative works of this code must retain this
# copyright notice, and modified files need to carry a notice indicating
# that they have been altered from the originals.


#BSUB -q night
#BSUB -n 4
#BSUB -Rspan[ptile=4]
#BSUB -o out.%J
#BSUB -gpu "num=1"
#BSUB -J mpi-cc

conda activate hpcqc

# Uncomment the lines relevant to the current experiment.
code="poster_mpi_4_hierarchical.py"
tasks=4
#code="poster_mpi_8_hierarchical.py"
#tasks=8
#for samples in 100 1000 10000 100000
for samples in 10000
do
	echo "-----------------------------------------------------------"
	echo "Running $code with $tasks tasks and $samples samples"
	mpiexec -n $tasks --map-by core python $code --num-samples $samples --shots 4096
	echo "-----------------------------------------------------------"
done
