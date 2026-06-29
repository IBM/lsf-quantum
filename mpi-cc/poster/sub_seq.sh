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
#BSUB -o out.%J
#BSUB -gpu "num=1"
#BSUB -J baseline

conda activate hpcqc

for samples in 100 1000 10000 100000
do
	echo "-------------------------"
	echo "Executing with $samples"
	./poster_sequential_baseline.py --num-samples $samples --shots 4095
	echo "-------------------------"
done	
