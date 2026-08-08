# QEMU in a container, so OS101 can be booted and screenshotted on hosts
# where installing QEMU natively is not an option (notably Apple Silicon
# without Homebrew).
#
# The base is pinned to an image already present locally: this machine's
# Docker daemon cannot resolve registry metadata, but container networking
# and the Alpine package CDN both work, so `apk add` succeeds where a pull
# would not.
#
# The container is arm64-native; `qemu-system-x86_64` emulates the guest in
# software via TCG, so there is no nested-virtualisation requirement.
FROM golang:1.26-alpine

RUN apk add --no-cache qemu-system-x86_64

WORKDIR /work
ENTRYPOINT ["/bin/sh", "-c"]
