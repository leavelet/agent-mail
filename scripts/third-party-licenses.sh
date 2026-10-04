#!/bin/sh
# Regenerate THIRD_PARTY_LICENSES.md: the licenses of the Rust crates in the
# `amail` binary (cargo-about, see about.toml) and of the npm packages that
# the web build bundles into it (web/dist).
#
# Needs: cargo-about (cargo install --locked cargo-about --features cli) and
# `npm ci` in web/.
set -eu
cd "$(dirname "$0")/.."
out=THIRD_PARTY_LICENSES.md

cargo about generate --manifest-path crates/amail-cli/Cargo.toml about.hbs > "$out.tmp"

# npm packages bundled into web/dist (the rest of web/package.json is build
# tooling). Check with `npx vite build --sourcemap` when dependencies change.
bundled="svelte"
{
    printf '## npm packages in the web interface\n\n'
    for p in $bundled; do
        dir="web/node_modules/$p"
        [ -d "$dir" ] || { echo "error: $dir missing; run npm ci in web/" >&2; exit 1; }
        version=$(node -p "require('./$dir/package.json').version")
        license=$(node -p "require('./$dir/package.json').license")
        file=
        for f in "$dir"/LICENSE* "$dir"/LICENCE* "$dir"/license* "$dir"/COPYING*; do
            if [ -f "$f" ]; then file=$f; break; fi
        done
        [ -n "$file" ] || { echo "error: no license file in $dir" >&2; exit 1; }
        printf '### %s %s (%s)\n\n```\n' "$p" "$version" "$license"
        cat "$file"
        printf '```\n\n'
    done
} >> "$out.tmp"
mv "$out.tmp" "$out"
echo "wrote $out"
