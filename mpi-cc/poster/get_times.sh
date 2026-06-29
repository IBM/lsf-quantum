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

function get_times_file()
{
  bench=$1
  file=$2
  case $bench in
	"base")
		command=$(grep 'execute=' $file |awk '{print $5}' |awk -F = '{print $2}' |sed 's/s//')	
	;;
	"mpi8")
		command=$(grep 'execute=' $file |awk '{print $7}' |awk -F = '{print $2}' |sed 's/s//')
	;;
	"mpi4")
		command=$(grep execute_level_two_tasks $file |awk '{print $7}')
	;;
	*)
	echo "Usage $0 base/mpi4/mpi8 <file_name>"
        exit 1 	
	;;
  esac

  echo "$command"
}  

max_args() {
    printf "%s\n" "$@" | awk 'NR==1 || $1 > max {max=$1} END {print max}'
}

for bench in base mpi4 mpi8 
do	
  for samples in 100 1000 10000 100000
  do
     file="out.$bench"_"$samples"
     vals=$(get_times_file $bench $file)
     max_val=$(max_args $vals)
     echo $bench $samples $max_val
     echo "                  "
  done   
done	
