# (C) Copyright 2025-2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

# Build schmod_qrmi.so, the LSF scheduler plug-in.
#
#   make LSF_INCDIR=/opt/lsf/10.1/include/lsf        # build
#   make test                                        # host-side harness (no LSF needed)
#   make install LSF_LIBDIR=/opt/lsf/10.1/linux3.10-glibc2.17-x86_64/lib
#
# Needs only the public lssched.h; the QPU selection policies come from the
# Rust crate qpu-select-ffi, linked in statically.

LSF_INCDIR ?= $(LSF_TOP)/10.1/include/lsf
CARGO      ?= cargo
FFI_DIR     = ../crates/qpu-select-ffi
FFI_LIB     = ../target/release/libqpu_select_ffi.a

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Linux)
  # What `cargo rustc -- --print native-static-libs` reports for the crate.
  NATIVE_LIBS ?= -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc
  # Keep the Rust runtime's symbols out of mbschd's global namespace.
  SHARED_LDFLAGS = -shared -Wl,--exclude-libs,ALL
else
  NATIVE_LIBS ?= -lSystem -lc -lm
  SHARED_LDFLAGS = -dynamiclib -undefined dynamic_lookup
endif

CFLAGS ?= -O2 -g
CFLAGS += -Wall -Wextra -fPIC -I$(LSF_INCDIR) -I$(FFI_DIR)/include

all: schmod_qrmi.so

$(FFI_LIB): $(wildcard $(FFI_DIR)/src/*.rs ../crates/qpu-select/src/*.rs)
	cd .. && $(CARGO) build --release --locked -p qpu-select-ffi

schmod_qrmi.o: schmod_qrmi.c $(FFI_DIR)/include/qrmi_select.h
	$(CC) $(CFLAGS) -c -o $@ schmod_qrmi.c

schmod_qrmi.so: schmod_qrmi.o $(FFI_LIB)
	$(CC) $(SHARED_LDFLAGS) -o $@ schmod_qrmi.o $(FFI_LIB) $(NATIVE_LIBS)

# The harness stubs the extsched_* calls, so it needs neither LSF nor its
# libraries -- only the header, for the type definitions.
test: test/harness
	./test/harness

test/harness: test/harness.c schmod_qrmi.c $(FFI_LIB)
	$(CC) -O0 -g -Wall -Wextra -I$(LSF_INCDIR) -I$(FFI_DIR)/include -I. \
	    -o $@ test/harness.c $(FFI_LIB) $(NATIVE_LIBS)

install: schmod_qrmi.so
	install -m 755 schmod_qrmi.so $(LSF_LIBDIR)/schmod_qrmi.so

clean:
	rm -rf *.o *.so test/harness test/harness.dSYM

.PHONY: all test install clean
