#!/bin/sh
# Build a .deb for vidwatcher. Needs: cargo, dpkg-deb, fakeroot.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)

version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
arch=$(dpkg --print-architecture)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
chmod 0755 "$stage"

echo ">> cargo build --release"
cargo build --release --manifest-path "$root/Cargo.toml"

echo ">> staging $version ($arch)"
install -Dm0755 "$root/target/release/vidwatcher"   "$stage/usr/bin/vidwatcher"
install -Dm0644 "$root/packaging/vidwatcher.service" "$stage/lib/systemd/system/vidwatcher.service"
install -Dm0644 "$root/packaging/config.toml"        "$stage/etc/vidwatcher/config.toml"
install -Dm0644 "$root/README.md"                    "$stage/usr/share/doc/vidwatcher/README.md"
install -Dm0644 "$root/packaging/config.toml"        "$stage/usr/share/doc/vidwatcher/config.toml.example"

mkdir -p "$stage/DEBIAN"
sed -e "s/^Version:.*/Version: $version/" \
    -e "s/^Architecture:.*/Architecture: $arch/" \
    "$root/packaging/debian/control" > "$stage/DEBIAN/control"

for s in postinst prerm postrm; do
    install -m0755 "$root/packaging/debian/$s" "$stage/DEBIAN/$s"
done

echo "/etc/vidwatcher/config.toml" > "$stage/DEBIAN/conffiles"

# md5sums for everything except DEBIAN/
( cd "$stage" && find . -path ./DEBIAN -prune -o -type f -printf '%P\0' \
    | xargs -0 md5sum > DEBIAN/md5sums )

out="$root/vidwatcher_${version}_${arch}.deb"
fakeroot dpkg-deb --build "$stage" "$out"
echo ">> built $out"
dpkg-deb --info "$out"
dpkg-deb --contents "$out"
