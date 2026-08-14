.PHONY: check test fmt storage-transport-spike worker-handshake

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

fmt:
	cargo fmt --all -- --check

storage-transport-spike:
	cargo run -p kide-core --example storage_transport_spike

worker-handshake:
	./workers/kotlin-jvm/gradlew --project-dir workers/kotlin-jvm run --args="--handshake"
