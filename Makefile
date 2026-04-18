.PHONY: setup

setup:
	@echo "🦅 Bootstrapping Crow workspace..."
	git config core.hooksPath .githooks
	@echo "✅ Git hooks configured."
	@echo "Run \`cargo build --workspace\` to verify dependencies."
