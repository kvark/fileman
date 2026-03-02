#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fileman_bin="${root_dir}/target/release/fileman"
cases_dir="${root_dir}/tests/cases"
out_dir="${root_dir}/tests/data/basic/out"

cd "${root_dir}"
cargo build --release --bin fileman

if [[ ! -x "${fileman_bin}" ]]; then
  echo "Missing fileman binary at ${fileman_bin}" >&2
  exit 1
fi

if [[ -d "${out_dir}" ]]; then
  find "${out_dir}" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
fi

shopt -s nullglob
cases=("${cases_dir}"/*.ron)
shopt -u nullglob
if [[ ${#cases[@]} -eq 0 ]]; then
  echo "No replay cases found in ${cases_dir}" >&2
  exit 1
fi

failures=0
for case in "${cases[@]}"; do
  "${fileman_bin}" --replay "${case}" || {
    echo "Replay failed: ${case}" >&2
    failures=$((failures + 1))
  }
done

if [[ ${failures} -ne 0 ]]; then
  echo "${failures} replay case(s) failed" >&2
  exit 1
fi
