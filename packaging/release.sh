#!/usr/bin/env bash
# Gera os pacotes de release em dist/: .deb (cargo-deb) e tarball dos binários.
# Uso: packaging/release.sh
# Publicar não é daqui: a tag v<versão> empurrada para o GitHub dispara
# .github/workflows/release.yml, que roda este script e sobe o GitHub Release.
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

