.DEFAULT_GOAL := help

.PHONY: help build macos windows test clean

help:
	@echo 'VPN Splitter build commands:'
	@echo '  make build    Build release binaries for this computer'
	@echo '  make macos    Build and sign the macOS .app bundle (requires macOS)'
	@echo '  make windows  Cross-build Windows binaries with MinGW and Bash'
	@echo '  make test     Run Rust and shell regression tests'
	@echo '  make clean    Remove Cargo build outputs'

build:
	cargo build --release --locked -p vpn-splitter-desktop -p vpn-splitter-helper

macos:
	bash scripts/package-macos.sh

windows:
	bash scripts/build-windows.sh

test:
	cargo test --release --locked
	bash test_vpn_splitter.sh

clean:
	cargo clean
