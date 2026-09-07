# Containerized test environment
Build your test environment as a Docker image

## Prerequisites
[Podman](https://podman.io/) is installed.
LSF CE distribution files, e.g. `lsfsce10.2.0.15-armv8.tar.Z lsfsce10.2.0.15-ppc64le.tar.Z lsfsce10.2.0.15-x86_64.tar.Z` are available in the build directory.

## Building image
Invoke `build_podman.sh` script:
```
Usage: ./build_podman.sh <architecture> <lsf_version>
       Example: ./build_podman.sh arm64 10.2.0.15
```
Check the build:
```
podman images
REPOSITORY                     TAG         IMAGE ID      CREATED         SIZE
localhost/lsf-ce               latest      e346e84257d3  24 minutes ago  4.02 GB
quay.io/rockylinux/rockylinux  9           bc3d7521d778  3 months ago    259 MB
```
Launch a container for your target architecture, e.g.:
```
podman run --rm -it --hostname lsfmaster localhost/lsf-ce:latest /bin/bash
```
Inside ethe container:
```
su - lsfadmin

bhosts
HOST_NAME          STATUS       JL/U    MAX  NJOBS    RUN  SSUSP  USUSP    RSV
lsfmaster          ok              -      4      0      0      0      0      0
```
