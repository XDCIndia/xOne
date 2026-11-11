# Release targets (XB-26). Both run locally; no CI is needed.
#   make release   XONE_RELEASE_KEY=<gpg key id>   needs a clean checkout at a tag
#   make docker                                    wraps the tarball that `make release` wrote
#   make test-release                              two-clone build, checksums, signature, provenance
VERSION ?= $(shell python3 -I -c 'import tomllib;print(tomllib.load(open("Cargo.toml","rb"))["workspace"]["package"]["version"])')

.PHONY: release docker test-release
release:
	ci/release.sh

docker:
	docker build -f Dockerfile --build-arg VERSION=$(VERSION) -t xone:$(VERSION) dist

test-release:
	ci/test-release.sh
