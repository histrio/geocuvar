# Variables
RUST_LOG = info
GIT_COMMIT_MESSAGE = Committing changes in ./site folder

# Targets
.PHONY: all build run cleanup commit push

all: run cleanup commit push

build:
	@echo "Building Rust application"
	cargo build --locked --release

run: build
	@echo "Running application with RUST_LOG=$(RUST_LOG)"
	RUST_LOG=$(RUST_LOG) ./target/release/geocuvar

cleanup:
	@echo "Removing files older than one year"
	find ./site/content/changesets -name "*.md" -mtime +365 -delete

commit:
	@echo "Adding changes in ./site folder"
	git add ./site
	@echo "Committing changes with message: $(GIT_COMMIT_MESSAGE)"
	git commit -m "$(GIT_COMMIT_MESSAGE)"

push:
	@echo "Pushing changes to remote repository"
	git push

# Usage information
help:
	@echo "Usage:"
	@echo "  make all        - Run cargo with logging, cleanup, commit, and push changes"
	@echo "  make build      - Build Rust application"
	@echo "  make run        - Build and run application with logging"
	@echo "  make cleanup    - Remove files older than one year"
	@echo "  make commit     - Commit changes in ./site folder"
	@echo "  make push       - Push changes to remote repository"
	@echo "  make help       - Display this help message"
