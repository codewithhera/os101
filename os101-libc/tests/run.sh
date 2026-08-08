#!/usr/bin/env bash
#
# Build and run the os101-libc host tests.
#
# The portable half of the library — the printf engine, string.h, ctype.h, the
# conversions, qsort and bsearch, malloc, and math.h — is compiled for this
# machine and linked into one program together with the tests, which include the
# *host's* headers and call the host's snprintf, strtod and libm as the
# reference. Two libcs in one program only works because every name the library
# defines is renamed to os101_* (see host_names.h), and this script checks that
# the renaming is complete before it runs anything: a name left out would
# silently replace the host function it was supposed to be compared against.
#
# What is not covered here needs the kernel, and is verified by inspecting the
# linked ELF and by the two example applications instead: crt0.S, the syscall
# wrappers in syscall.c, the GUI calls in os101.c, time.c, init.c's walk of
# .init_array, and the C++ runtime in cxxrt.cpp.
#
set -euo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIBC="$(cd "${TESTS_DIR}/.." && pwd)"
ROOT="$(cd "${LIBC}/.." && pwd)"
OUT="${ROOT}/build/os101-libc-tests"

CC="${CC:-}"
if [ -z "${CC}" ]; then
  if command -v clang >/dev/null 2>&1; then
    CC="$(command -v clang)"
  elif command -v cc >/dev/null 2>&1; then
    CC="$(command -v cc)"
  else
    echo "tests: no C compiler found; set CC" >&2
    exit 1
  fi
fi

mkdir -p "${OUT}"

# The library sources that are pure computation. The rest (crt0.S, syscall.c,
# os101.c, time.c, init.c, cxxrt.cpp) is the ABI itself and cannot run here.
LIBRARY_SOURCES=(
  decimal.c
  stdio.c
  string.c
  ctype.c
  stdlib.c
  malloc.c
  math.c
  errno.c
  assert.c
)

TEST_SOURCES=(
  harness.c
  host_stubs.c
  test_printf.c
  test_string.c
  test_stdlib.c
  test_malloc.c
  test_math.c
)

# -fno-builtin for the same reason the target build needs it: without it the
# copy loop in string.c is recognised as a memcpy and compiled into a call to
# itself. -include host_names.h is what does the renaming.
LIBRARY_FLAGS=(
  -std=c11
  -O1
  -g
  -fno-builtin
  -fno-strict-aliasing
  -ffreestanding
  -Wall
  -Wextra
  -Werror
  -Wno-unused-parameter
  "-isystem${LIBC}/include"
  "-include${TESTS_DIR}/host_names.h"
)

TEST_FLAGS=(
  -std=c11
  -O1
  -g
  -Wall
  -Wextra
  -Werror
  -Wno-unused-parameter
  "-I${TESTS_DIR}"
)

OBJECTS=()

echo "building the library for this machine"
for src in "${LIBRARY_SOURCES[@]}"; do
  obj="${OUT}/lib_${src%.c}.o"
  "${CC}" "${LIBRARY_FLAGS[@]}" -c "${LIBC}/src/${src}" -o "${obj}"
  OBJECTS+=("${obj}")
done

# Every external symbol the library defines has to be os101_-prefixed. nm marks
# defined externals with an uppercase letter and undefined ones with U, and Mach-O
# puts an underscore in front of every name.
echo "checking that no library symbol can collide with the host's libc"
BAD="$(nm -g "${OBJECTS[@]}" \
  | awk '$2 ~ /^[TDBSCRI]$/ { print $3 }' \
  | grep -v '^_os101_' \
  | sort -u || true)"
if [ -n "${BAD}" ]; then
  echo "tests: these symbols are not renamed, so they would override the host's:" >&2
  echo "${BAD}" | sed 's/^/  /' >&2
  echo "tests: add them to os101-libc/tests/host_names.h" >&2
  exit 1
fi

echo "building the tests"
for src in "${TEST_SOURCES[@]}"; do
  obj="${OUT}/${src%.c}.o"
  "${CC}" "${TEST_FLAGS[@]}" -c "${TESTS_DIR}/${src}" -o "${obj}"
  OBJECTS+=("${obj}")
done

"${CC}" -o "${OUT}/os101-libc-tests" "${OBJECTS[@]}" -lm

echo
"${OUT}/os101-libc-tests"
status=$?
echo
if [ "${status}" -eq 0 ]; then
  echo "os101-libc: all host tests passed"
else
  echo "os101-libc: TESTS FAILED"
fi
exit "${status}"
