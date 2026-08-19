.PHONY: build check test e2e verify full_build fmt storage-transport-spike worker-handshake

KIDE_MAVEN_HOME ?= $(HOME)/.sdkman/candidates/maven/current
export KIDE_MAVEN_HOME

build:
	cargo build
	./workers/kotlin-jvm/gradlew --project-dir workers/kotlin-jvm installDist

check:
	cargo check --workspace --all-targets
	cargo clippy --workspace --all-targets -- -D warnings
	./workers/kotlin-jvm/gradlew --project-dir workers/kotlin-jvm check

test:
	cargo test --workspace
	./workers/kotlin-jvm/gradlew --project-dir workers/kotlin-jvm test

e2e: build
	cargo test -p kide --test mvp_e2e -- --ignored --nocapture

verify: check test e2e

full_build: verify

fmt:
	cargo fmt --all -- --check

storage-transport-spike:
	cargo run -p kide-core --example storage_transport_spike

worker-handshake:
	./workers/kotlin-jvm/gradlew --project-dir workers/kotlin-jvm run --args="--handshake"
