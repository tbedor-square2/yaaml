set shell := ["bash", "-cu"]

default: quality

# Run the full local quality gate after code changes.
quality:
    scripts/check-quality.sh

# Alias for users who naturally reach for `just check`.
check: quality

