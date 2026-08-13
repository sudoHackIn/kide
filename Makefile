.PHONY: check test fmt storage-transport-spike worker-handshake

check:
	cargo check --workspace --all-targets
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
