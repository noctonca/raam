#!/usr/bin/env bash
# The golden suite: every preset's --exact shot from the desktop host,
# pinned as a hash of its pixels in tests/goldens.txt. Pixels are exact
# per GPU and driver, so the hashes hold on the kind of machine that
# blessed them (the file's header names it), not in CI.
#
#   scripts/goldens.sh [check] [GLOB...]   hash every shot (or those whose
#                                          names match) and compare; for a
#                                          shot that differs, draw it at the
#                                          last blessed commit and now
#   scripts/goldens.sh bless               rewrite tests/goldens.txt from
#                                          this tree; commit it on its own
#   scripts/goldens.sh show GLOB...        draw shots at the last blessed
#                                          commit and now, and diff them
#   scripts/goldens.sh list                every shot's name and flags
#
# Pictures go to target/goldens/: NAME.before.png (the blessed commit,
# built in a worktree under target/goldens/), NAME.after.png (this tree)
# and NAME.diff.png (after in grey, the differing pixels magenta).
#
# The suite: every gallery page (but the live probe) and every frame_ui
# page with each of its fixtures, in both themes (the menu over black in
# the dark theme and over white in the light, its worst cases), plus a
# few shots that exercise the flags (scroll, a tap, typing, a smaller
# screen). Every shot is a hermetic run (src/preset.rs), so neither the
# load nor the display nor the mouse can change it.
set -uo pipefail
# The same sort order on every machine.
export LC_ALL=C

cd "$(git rev-parse --show-toplevel)" || exit 2
GOLDENS=tests/goldens.txt
OUT=target/goldens
# The `quick` profile: release without LTO, the same pixels for a
# fraction of the relink.
BIN=target/quick/raam
# Each shot is a process of its own; one per core is fastest (on an M1
# Pro: 23 s at 10, 25 s at 8, 36 s at 4).
JOBS=$(getconf _NPROCESSORS_ONLN)

# name, then the flags that draw it (--exact and --hash are added).
list() {
    local page theme
    for page in $("$BIN" --pages); do
        # Its numbers are live measurements (fps, memory).
        [ "$page" = probe ] && continue
        for theme in dark light; do
            printf '%s-%s --page %s %s\n' "$page" "$theme" "$page" "$(look "$theme")"
        done
    done
    for theme in dark light; do
        printf '%s-%s %s %s\n' \
            set-albums-scrolled "$theme" "--page set-albums --scroll 400" "$(look "$theme")" \
            set-display-tapped "$theme" "--page set-display --click 1179,353" "$(look "$theme")" \
            set-server-typed "$theme" "--page set-server-keyboard --click 1083,716" "$(look "$theme")" \
            menu-1024x600 "$theme" "--page menu --size 1024x600" "$(look "$theme")" \
            set-photos-1024x600 "$theme" "--page set-photos --size 1024x600" "$(look "$theme")"
    done
}

look() {
    if [ "$1" = dark ]; then
        echo "--theme dark --backdrop black"
    else
        echo "--theme light --backdrop white"
    fi
}

# The list, narrowed to names matching any of the globs.
pick() {
    local name flags g
    while read -r name flags; do
        if [ $# -eq 0 ]; then
            echo "$name $flags"
            continue
        fi
        for g in "$@"; do
            # shellcheck disable=SC2254
            case "$name" in $g) echo "$name $flags"; break ;; esac
        done
    done
}

# One draw, guarded: a shot run exits by itself once egui settles.
draw() {
    local bin=$1
    shift
    perl -e 'alarm shift; exec @ARGV' 60 "$bin" "$@" --exact
}

# Hashes the shots on stdin, JOBS at a time: "name size hash" per line,
# sorted, "name - FAILED" for a run that printed none. The GL line of one
# run goes to $OUT/gl.txt and every run's window scale to $OUT/scales.txt.
hash_all() {
    local list k
    list=$(mktemp)
    cat > "$list"
    rm -f "$OUT"/hash.* "$OUT/scales.txt"
    for k in $(seq 0 $((JOBS - 1))); do
        awk -v j="$JOBS" -v k="$k" 'NR % j == k' "$list" | while read -r name flags; do
            # shellcheck disable=SC2086
            if out=$(draw "$BIN" $flags --hash 2> "$OUT/log.$k") && [ -n "$out" ]; then
                echo "$name $out"
            else
                echo "$name - FAILED"
                sed 's/^/    /' "$OUT/log.$k" >&2
            fi
            grep -o 'GL .*| GL_MAX' "$OUT/log.$k" | head -1 > "$OUT/gl.$k"
            grep -o 'window scale [0-9.]*' "$OUT/log.$k" >> "$OUT/scales.$k"
        done > "$OUT/hash.$k" &
    done
    wait
    cat "$OUT"/scales.* 2> /dev/null | sort -u > "$OUT/scales.txt"
    cat "$OUT"/gl.* 2> /dev/null | grep -m1 . | sed 's/ | GL_MAX$//' > "$OUT/gl.txt"
    rm -f "$OUT"/scales.? "$OUT"/gl.? "$OUT"/log.? "$list"
    sort "$OUT"/hash.*
    rm -f "$OUT"/hash.*
}

# "Apple M1 Pro, GL 4.1 Metal - 90.5" from "GL 4.1 Metal - 90.5 | Apple
# M1 Pro | window scale 2".
renderer() {
    awk -F' [|] ' '{ print $2 ", " $1 }' "$OUT/gl.txt"
}

build() {
    echo "building the desktop host (quick)"
    cargo build --profile quick --bin raam -q || exit 2
}

# The commit that last blessed, built in a worktree of its own; echoes the
# binary's path.
base_bin() {
    local base wt=$OUT/base
    base=$(git log -1 --format=%H -- "$GOLDENS")
    if [ -z "$base" ]; then
        echo "no commit has blessed $GOLDENS yet" >&2
        return 1
    fi
    git worktree prune
    if [ -d "$wt" ]; then
        git -C "$wt" checkout -q --detach "$base" || return 1
    else
        git worktree add -q --detach "$wt" "$base" || return 1
    fi
    # A copy-on-write clone of this tree's build, where the filesystem
    # has them, so only what differs compiles.
    if [ ! -d "$OUT/base-target" ]; then
        mkdir -p "$OUT/base-target"
        cp -cR target/quick "$OUT/base-target/" 2> /dev/null || true
    fi
    echo "building the blessed commit ${base:0:8} in $wt" >&2
    # The profile is spelled out here: a commit blessed before it existed
    # doesn't define it.
    CARGO_TARGET_DIR=$OUT/base-target cargo build --profile quick --bin raam -q \
        --config 'profile.quick.inherits="release"' --config profile.quick.lto=false \
        --manifest-path "$wt/Cargo.toml" >&2 || return 1
    echo "$OUT/base-target/quick/raam"
}

# Draws each shot on stdin before (the blessed commit) and after (this
# tree), and diffs them.
show() {
    local before name flags stored
    before=$(base_bin) || before=
    while read -r name flags; do
        # shellcheck disable=SC2086
        draw "$BIN" $flags --screenshot "$OUT/$name.after.png" 2> /dev/null
        if [ -z "$before" ]; then
            echo "  $name: $OUT/$name.after.png (nothing to compare with)"
            continue
        fi
        # shellcheck disable=SC2086
        got=$(draw "$before" $flags --hash --screenshot "$OUT/$name.before.png" 2> /dev/null)
        stored=$(awk -v n="$name" '$1 == n { print $2 " " $3 }' "$GOLDENS")
        printf '  %s: %s\n' "$name" \
            "$("$BIN" --diff "$OUT/$name.before.png" "$OUT/$name.after.png" \
                --out "$OUT/$name.diff.png")"
        if [ -n "$stored" ] && [ "$got" != "$stored" ]; then
            echo "    (the blessed commit draws differently here than where it was blessed)"
        fi
    done
}

mkdir -p "$OUT"
cmd=${1:-check}
[ $# -gt 0 ] && shift
case "$cmd" in
list)
    build > /dev/null
    list | pick "$@"
    ;;
bless)
    [ $# -eq 0 ] || { echo "bless takes no names: the file is always the whole suite" >&2; exit 2; }
    build
    shots=$(list)
    echo "hashing $(echo "$shots" | wc -l | tr -d ' ') shots"
    result=$(echo "$shots" | hash_all)
    if echo "$result" | grep -q ' FAILED$'; then
        echo "$result" | grep ' FAILED$'
        echo "not blessed: some shots failed"
        exit 1
    fi
    mkdir -p "$(dirname "$GOLDENS")"
    {
        echo "# Raam's golden suite: a hash of every preset's --exact shot, one"
        echo "# per line (name, size, FNV-1a 64 of the pixels). scripts/goldens.sh"
        echo "# checks and blesses it. Blessed on $(renderer)."
        echo "$result"
    } > "$GOLDENS"
    echo "blessed $(echo "$result" | wc -l | tr -d ' ') shots on $(renderer) ($(tr '\n' ' ' < "$OUT/scales.txt" | sed 's/ $//'))"
    git --no-pager diff --stat -- "$GOLDENS"
    ;;
check)
    [ -f "$GOLDENS" ] || { echo "no $GOLDENS: run scripts/goldens.sh bless" >&2; exit 2; }
    build
    shots=$(list | pick "$@")
    [ -n "$shots" ] || { echo "no shot matches $*" >&2; exit 2; }
    echo "hashing $(echo "$shots" | wc -l | tr -d ' ') shots"
    result=$(echo "$shots" | hash_all)
    echo "drew on $(renderer) ($(tr '\n' ' ' < "$OUT/scales.txt" | sed 's/ $//'))"
    echo "blessed on $(sed -n 's/^# .*Blessed on \(.*\)\.$/\1/p' "$GOLDENS")"
    # Changed, new, and (for a whole run) stale: in the file, not the list.
    report=$(awk -v whole=$(( $# == 0 )) '
        FNR == NR { if ($0 !~ /^#/) stored[$1] = $2 " " $3; next }
        { seen[$1] = 1
          if (!($1 in stored)) print "new     " $1
          else if (stored[$1] != $2 " " $3) print "differs " $1 }
        END { if (whole) for (n in stored) if (!(n in seen)) print "stale   " n }
    ' "$GOLDENS" - <<< "$result" | sort -k2)
    if [ -z "$report" ]; then
        echo "all $(echo "$result" | wc -l | tr -d ' ') shots match"
        exit 0
    fi
    while read -r line; do echo "  $line"; done <<< "$report"
    names=$(echo "$report" | awk '$1 != "stale" { print $2 }')
    if [ -n "$names" ]; then
        echo "drawing them before and after, into $OUT/"
        # shellcheck disable=SC2086
        list | pick $names | show
    fi
    echo "if the change is meant, bless it in a commit of its own: scripts/goldens.sh bless"
    exit 1
    ;;
show)
    [ $# -gt 0 ] || { echo "show which shots? (scripts/goldens.sh list)" >&2; exit 2; }
    build
    list | pick "$@" | show
    ;;
*)
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
