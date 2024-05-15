# Variables
RUST_LOG = info
GIT_COMMIT_MESSAGE = Committing changes in ./site folder

# Targets
.PHONY: all run commit push

all: run commit push

run:
	@echo "Running cargo with RUST_LOG=$(RUST_LOG)"
	RUST_LOG=$(RUST_LOG) cargo run --release

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
	@echo "  make all     - Run cargo with logging, commit, and push changes"
	@echo "  make run     - Run cargo with logging"
	@echo "  make commit  - Commit changes in ./site folder"
	@echo "  make push    - Push changes to remote repository"
	@echo "  make help    - Display this help message"
