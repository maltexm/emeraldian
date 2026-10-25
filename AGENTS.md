# Agent rules

- Never run emeraldian against the real Obsidian CLI; the repo sets `OBSIDIAN_CLI=off` (`.cargo/config.toml`), keep it when running the built binary by hand (`OBSIDIAN_CLI=off target/debug/emeraldian`).
