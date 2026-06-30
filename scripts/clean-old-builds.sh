#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: clean-old-builds.sh [--days N] [--root PATH] [--dry-run|--apply]

Clean old Cargo build artifacts with cargo-sweep.

Options:
  --days N     Keep artifacts newer than N days. Default: 7.
  --root PATH  Root to scan recursively. Default: ../yaaml-worktrees.
  --dry-run    Preview removals without deleting. Default.
  --apply      Delete matching old build artifacts.
  -h, --help   Show this help.
USAGE
}

days=7
mode="--dry-run"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
default_root="$(cd -- "$script_dir/../.." && pwd)/yaaml-worktrees"
root="$default_root"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --days)
      [[ $# -ge 2 ]] || { echo "missing value for --days" >&2; exit 2; }
      days="$2"
      shift 2
      ;;
    --root)
      [[ $# -ge 2 ]] || { echo "missing value for --root" >&2; exit 2; }
      root="$2"
      shift 2
      ;;
    --dry-run)
      mode="--dry-run"
      shift
      ;;
    --apply)
      mode=""
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if ! [[ "$days" =~ ^[0-9]+$ ]]; then
  echo "--days must be a non-negative integer" >&2
  exit 2
fi

if [[ ! -d "$root" ]]; then
  echo "root does not exist: $root" >&2
  exit 1
fi

if ! command -v cargo-sweep >/dev/null 2>&1 && ! cargo sweep --help >/dev/null 2>&1; then
  echo "cargo-sweep is not installed. Install with: cargo install cargo-sweep" >&2
  exit 1
fi

if [[ -n "$mode" ]]; then
  echo "Dry run: old Cargo build artifacts under $root older than $days days"
  cargo sweep --recursive --dry-run --time "$days" "$root"
else
  echo "Applying cleanup: old Cargo build artifacts under $root older than $days days"
  cargo sweep --recursive --time "$days" "$root"
fi
