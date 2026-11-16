#!/bin/sh
# Install a built `forge` binary into a directory on your PATH.
#
# Copies a binary you already have; downloads nothing. Run it after
# `cargo build --release` (or point --source at a release artifact).
#
#   sh scripts/install.sh                          # copy target/release/forge
#   sh scripts/install.sh --source dist/forge
#   sh scripts/install.sh --add-to-path            # append to ~/.profile once
#
# Exits 0 when the installed copy answers --version, 1 otherwise.

set -eu

source_path=""
prefix=""
add_to_path=0
force=0

while [ $# -gt 0 ]; do
    case "$1" in
        --source) source_path="${2:?--source needs a path}"; shift 2 ;;
        --prefix) prefix="${2:?--prefix needs a path}"; shift 2 ;;
        --add-to-path) add_to_path=1; shift ;;
        --force) force=1; shift ;;
        -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "install: unknown argument $1" >&2; exit 2 ;;
    esac
done

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if [ -z "$source_path" ]; then
    for candidate in "$here/../target/release/forge" "$here/../bin/forge"; do
        if [ -f "$candidate" ]; then
            source_path=$candidate
            break
        fi
    done
fi
if [ -z "$source_path" ] || [ ! -f "$source_path" ]; then
    echo "install: no forge binary found; build one with 'cargo build --release' or pass --source" >&2
    exit 1
fi

# Read the version before touching anything, so a wrong file fails here.
source_version=$("$source_path" --version 2>&1 | head -n 1)
case "$source_version" in
    forge\ *) ;;
    *) echo "install: $source_path does not look like forge: '$source_version'" >&2; exit 1 ;;
esac

if [ -z "$prefix" ]; then
    prefix="${HOME}/.local/bin"
fi
mkdir -p "$prefix"
target="$prefix/forge"

if [ -e "$target" ] && [ "$force" -eq 0 ]; then
    # A running binary can be replaced on Unix; the flag exists anyway so the
    # two installers behave the same and an operator who wants a clean run can
    # ask for it.
    :
fi

cp "$source_path" "$target"
chmod 755 "$target"

installed_version=$("$target" --version 2>&1 | head -n 1)
if [ "$installed_version" != "$source_version" ]; then
    echo "install: installed copy reports '$installed_version', expected '$source_version'" >&2
    exit 1
fi

echo "source:    $source_path"
echo "version:   $source_version"
echo "installed: $target"

case ":${PATH}:" in
    *":${prefix}:"*) echo "path:      already on PATH" ;;
    *)
        if [ "$add_to_path" -eq 1 ]; then
            profile="${HOME}/.profile"
            touch "$profile"
            if ! grep -F "forge install: $prefix" "$profile" >/dev/null 2>&1; then
                # Idempotent: the marker makes a second run a no-op, so nobody
                # ends up with the same directory prepended five times.
                printf '\n# forge install: %s\nexport PATH="%s:$PATH"\n' "$prefix" "$prefix" >>"$profile"
                echo "path:      appended $prefix to $profile (new shells only)"
            else
                echo "path:      $profile already exports $prefix"
            fi
        else
            echo "path:      not on PATH; add this to your shell profile:"
            echo "           export PATH=\"$prefix:\$PATH\""
        fi
        ;;
esac

echo ""
echo "next: forge doctor"