set shell := ["bash", "-lc"]

root := justfile_directory()

default:
    @just --list

bootstrap:
    cargo fetch
    cd apps/macos && xcodegen generate

build: build-rust build-app

build-rust:
    cargo build --workspace

build-app:
    cd apps/macos && xcodegen generate
    cd apps/macos && xcodebuild -project VerlinPark.xcodeproj -scheme VerlinPark -configuration Debug -destination 'platform=macOS' -derivedDataPath build CODE_SIGNING_ALLOWED=NO build

run: build
    open apps/macos/build/Build/Products/Debug/VerlinPark.app

bundle: build
    scripts/bundle-daemon.sh

daemon:
    cargo run -p vp-daemon

cli *args:
    cargo run -q -p vp-cli -- {{args}}

test:
    cargo test --workspace
    cd apps/macos && xcodebuild -project VerlinPark.xcodeproj -scheme VerlinPark -configuration Debug -destination 'platform=macOS' -derivedDataPath build CODE_SIGNING_ALLOWED=NO test

fmt:
    cargo fmt --all
    swiftformat apps/macos/VerlinPark apps/macos/VerlinParkTests

lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    swiftformat --lint apps/macos/VerlinPark apps/macos/VerlinParkTests

clean:
    cargo clean
    rm -rf apps/macos/build apps/macos/VerlinPark.xcodeproj
