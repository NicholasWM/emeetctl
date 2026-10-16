#!/usr/bin/env bash
# Gera os pacotes de release em dist/: .deb (cargo-deb) e tarball dos binários.
# Uso: packaging/release.sh            (só gera)
#      packaging/release.sh --publish  (gera e publica no GitHub Releases com tag v<versão>)
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
NAME="emeetctl-${VERSION}-x86_64-linux"

cargo test --workspace --quiet
cargo build --release --workspace
cargo deb -p emeetctl-gui --no-build

rm -rf dist && mkdir -p "dist/${NAME}"
cp target/release/{emeetctl,emeetctl-gui,emeetctl-mcp} "dist/${NAME}/"
cp -r udev emeetctl.desktop README.md LICENSE-MIT LICENSE-APACHE "dist/${NAME}/"
[ -d assets/icons ] && cp -r assets/icons "dist/${NAME}/"
tar -C dist -czf "dist/${NAME}.tar.gz" "${NAME}" && rm -rf "dist/${NAME}"
cp target/debian/emeetctl_"${VERSION}"-1_amd64.deb dist/
(cd dist && sha256sum ./* > SHA256SUMS)
ls -la dist

if [ "${1:-}" = "--publish" ]; then
    git tag -a "v${VERSION}" -m "v${VERSION}" 2>/dev/null || true
    git push github "v${VERSION}"
    gh release create "v${VERSION}" dist/* --repo NicholasWM/emeetctl --title "emeetctl ${VERSION}" --generate-notes
fi
