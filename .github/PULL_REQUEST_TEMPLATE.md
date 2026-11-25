## Summary

<!-- Briefly describe what changed and why. Link related issue(s), e.g., Closes #123 -->

## Architecture & Safety Checklist

- [ ] Domain logic and orchestration live in `wiki-core` (`ops.rs`), keeping `wiki-cli` and `wiki-mcp` thin and identical in behavior
- [ ] If an MCP tool was added/changed, it acquires `WikiServer::op_lock` and never writes to `stdout`
- [ ] All workspace filesystem paths go through `Workspace` helpers (`abs` / `rel` / `read_file` / `write_page`)
- [ ] Unit tests added or updated in `crates/wiki-core` using temporary workspaces (`tempfile`)
- [ ] Documentation updated (`README.md` and `README.zh-CN.md`) if user-facing flags or MCP schemas changed

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo doc --workspace --no-deps`
