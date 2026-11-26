#!/bin/sh
# Run inside an ubuntu container: prints fixture files into /out. Usage: capture.sh <tag>
set -eu
tag=$1
export DEBIAN_FRONTEND=noninteractive LC_ALL=C
apt-get update -qq >/dev/null
apt-get install -y -qq hello jq >/dev/null 2>&1
apt-get remove -y -qq jq >/dev/null 2>&1
apt-mark hold hello >/dev/null
/usr/bin/dpkg-query -W -f='${Package}\t${Version}\t${db:Status-Abbrev}\n' > /out/dpkg-query.$tag.txt
/usr/bin/apt-cache policy hello jq nosuchpkg-xyz bash > /out/apt-cache-policy.$tag.txt 2>/dev/null || true
/usr/bin/dpkg-query -S /usr/bin/hello /usr/bin/nosuch > /out/dpkg-S.$tag.txt 2>/dev/null || true
