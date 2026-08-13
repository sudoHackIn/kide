.PHONY: check test fmt worker-handshake

check:
	cargo check --workspace --all-targets
	./workers/kotlin-jvm/gradlew --project-dir workers/kotlin-jvm check

test:
	cargo test --workspace
	./workers/kotlin-jvm/gradlew --project-dir workers/kotlin-jvm test

fmt:
	cargo fmt --all -- --check

worker-handshake:
	./workers/kotlin-jvm/gradlew --project-dir workers/kotlin-jvm run --args="--handshake"
