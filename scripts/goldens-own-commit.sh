#!/usr/bin/env bash
# The golden rule's "own commit" half: a commit that touches
# tests/goldens.txt touches nothing else. goldens.sh draws its "before"
# shots at the last commit that blessed the file, so a bless mixed in
# with code would make that commit the baseline for the code it brought.
# Whether a pixel change was blessed at all stays a check by hand
# (goldens.sh check, on the Mac that blessed it).
#
#   scripts/goldens-own-commit.sh [BASE]   check every commit in BASE..HEAD
#                                          (default origin/main); CI runs
#                                          it over a pull request's commits
set -uo pipefail

GOLDENS=tests/goldens.txt
base=${1:-origin/main}

if ! git rev-parse --verify --quiet "$base^{commit}" > /dev/null; then
    echo "no commit $base to check from" >&2
    exit 2
fi

bad=0
for commit in $(git rev-list --no-merges "$base..HEAD"); do
    paths=$(git diff-tree --no-commit-id --name-only -r "$commit")
    if grep -qxF "$GOLDENS" <<< "$paths" && [ "$(wc -l <<< "$paths")" -gt 1 ]; then
        echo "$(git log -1 --format='%h %s' "$commit") touches $GOLDENS and:" >&2
        grep -vxF "$GOLDENS" <<< "$paths" | sed 's/^/  /' >&2
        bad=1
    fi
done

if [ "$bad" -ne 0 ]; then
    echo "bless in a commit of its own (docs/BUILDING.md#goldens)" >&2
    exit 1
fi
echo "every bless in $base..HEAD is a commit of its own"
