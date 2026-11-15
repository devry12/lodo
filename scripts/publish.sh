#!/usr/bin/env bash
# Tanda tangani dan unggah rilis ke R2 (bucket lodo-releases).
#
#   scripts/publish.sh <folder> <versi> [catatan]
#
# Butuh kunci rahasia di LODO_SIGNING_KEY (base64) atau LODO_SIGNING_KEY_FILE
# (default ~/.config/lodo/signing.key), dan wrangler yang sudah login
# (atau CLOUDFLARE_API_TOKEN + CLOUDFLARE_ACCOUNT_ID di CI).
set -euo pipefail
dir=$1
ver=${2#v}
notes=${3:-}
bucket=${LODO_BUCKET:-lodo-releases}
wrangler=(npx -y wrangler@4)

if [ -z "${LODO_SIGNING_KEY:-}" ] && [ -z "${LODO_SIGNING_KEY_FILE:-}" ]; then
  export LODO_SIGNING_KEY_FILE="$HOME/.config/lodo/signing.key"
fi

cargo run -q --release -p lodo-release -- manifest "$dir" "$ver" "$notes"

for f in "$dir"/lodo-*; do
  "${wrangler[@]}" r2 object put "$bucket/releases/$ver/$(basename "$f")" --file "$f" --remote
done
# Manifest terakhir, supaya klien tidak melihat versi baru sebelum filenya ada.
"${wrangler[@]}" r2 object put "$bucket/latest.json" --file "$dir/latest.json" --content-type application/json --remote
"${wrangler[@]}" r2 object put "$bucket/latest.json.sig" --file "$dir/latest.json.sig" --content-type text/plain --remote
echo "Rilis $ver terbit: ${LODO_UPDATE_URL:-https://lodo.kreasimaju.id}"
