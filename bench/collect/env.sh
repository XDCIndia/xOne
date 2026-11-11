#!/usr/bin/env bash
# Capture the environment fields of docs/perf.md section 2 into one JSON record on stdout.
# Read-only. Usage: env.sh --datadir DIR --binary PATH [--pid PID] [--cgroup /sys/fs/cgroup/PATH] > env.json
# Fields that cannot be read are written as null; the run is invalid until the owner fills them.
set -u
datadir=""; binary=""; pid=""; cg=""
while [ $# -gt 0 ]; do
  case "$1" in
    --datadir) datadir=$2; shift 2;;
    --binary) binary=$2; shift 2;;
    --pid) pid=$2; shift 2;;
    --cgroup) cg=$2; shift 2;;
    *) echo "unknown arg $1" >&2; exit 2;;
  esac
done
[ -n "$datadir" ] && [ -n "$binary" ] || { echo "need --datadir and --binary" >&2; exit 2; }
[ -z "$cg" ] && [ -n "$pid" ] && cg="/sys/fs/cgroup$(sed -n 's/^0:://p' "/proc/$pid/cgroup" 2>/dev/null)"

# Each section is captured as raw text so nothing is interpreted; python -I turns it into JSON.
export E_LSCPU="$(lscpu 2>&1)"
export E_NUMA="$(numactl --hardware 2>&1)"
export E_GOV="$(cpupower frequency-info 2>&1 || cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>&1 | sort | uniq -c)"
export E_BOOST="$(cat /sys/devices/system/cpu/cpufreq/boost /sys/devices/system/cpu/intel_pstate/no_turbo 2>&1)"
export E_SMT="$(cat /sys/devices/system/cpu/smt/active 2>&1)"
export E_UNAME="$(uname -a)"
export E_THP="$(cat /sys/kernel/mm/transparent_hugepage/enabled 2>&1)"
export E_SWAP="$(cat /proc/swaps 2>&1)"
export E_DF="$(df -hT -- "$datadir" 2>&1)"
export E_MOUNT="$(findmnt -T "$datadir" -o SOURCE,FSTYPE,OPTIONS 2>&1)"
dev="$(findmnt -n -T "$datadir" -o SOURCE 2>/dev/null)"
export E_BLK="$(lsblk -d -o NAME,MODEL,SIZE,ROTA,TRAN "$dev" 2>&1)"
export E_VERSION="$("$binary" --version 2>&1)"
export E_SHA="$(sha256sum -- "$binary" 2>&1)"
export E_CG_MAX="$([ -n "$cg" ] && cat "$cg/memory.max" 2>&1)"
export E_CG_CUR="$([ -n "$cg" ] && cat "$cg/memory.current" 2>&1)"
export E_CG_STAT="$([ -n "$cg" ] && cat "$cg/memory.stat" 2>&1)"
export E_CG_CPU="$([ -n "$cg" ] && cat "$cg/cpu.max" 2>&1)"
export E_RSS="$([ -n "$pid" ] && grep -E '^(VmRSS|RssAnon|RssFile|RssShmem)' "/proc/$pid/status" 2>&1)"
export E_NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
export E_DATADIR="$datadir" E_BINARY="$binary"

python3 -I - <<'PY'
import json, os
print(json.dumps({k[2:].lower(): v for k, v in os.environ.items() if k.startswith("E_")}, indent=2, sort_keys=True))
PY
