# Contributing to nuphus-mcp

Thanks for your interest in `nuphus-mcp`! This repository contains the MCP
Server that exposes Nuphus's desktop and browser automation as standard MCP
tools, plus the two crates it depends on.

## Development Environment

### Required tools

- **Rust** 1.78+ ([rustup](https://rustup.rs/))
- **Git**
- **Windows**: MSVC toolchain (the `desktop-api` crate uses Win32 APIs on
  Windows)
- **macOS**: Accessibility permission for the host application is required for
  desktop input; clipboard uses `arboard`.

### Quick start

```bash
git clone https://github.com/mrpulor-gh/nuphus-mcp.git
cd nuphus-mcp

# check the workspace compiles
cargo check --workspace

# run tests
cargo test -p nuphus-mcp
```

## Repository Layout

```
nuphus-mcp/
├── Cargo.toml                  # workspace root (3 crates)
├── crates/
│   ├── nuphus-mcp/             # MCP Server (protocol / server / tools / security)
│   ├── nuphus-browser/         # Browser automation core (CDP, chromiumoxide)
│   └── desktop-api/            # Desktop control core (vendored; xcap + Win32)
├── TOOLS.md                    # 37-tool reference (EN)
├── TOOLS.zh-CN.md              # 37-tool reference (ZH)
├── examples/                   # demo.rs lives in crates/nuphus-mcp/examples
└── ...
```

## Development Workflow

1. Fork the repository and create a feature branch.
2. Make changes with focused commits.
3. Ensure the workspace builds and tests pass:

   ```bash
   cargo check --workspace
   cargo test -p nuphus-mcp
   ```

4. If you changed tool schemas (`crates/nuphus-mcp/src/tools/schemas.rs`),
   update `TOOLS.md` / `TOOLS.zh-CN.md` accordingly — the docs are generated
   from the actual schema definitions.
5. Open a pull request describing the change and the evidence (build/test
   output).

## Rules

- **Do not** claim unsupported capabilities: the server only exposes the tools
  listed in `TOOLS.md` (generated from the actual schema definitions). Exposed
  tools must be genuinely implemented in this repository, never stubs.
- **Do not** introduce a runtime dependency on the Nuphus main application.
  This repository must remain self-contained (the only vendored crate is
  `desktop-api`, which has no main-repo internal dependencies).
- Keep `desktop-api` vendored as-is unless you also publish the fix upstream
  to the Nuphus main repository.
- Logs go to **stderr**; stdout is reserved for the JSON-RPC protocol.

## License

By contributing, you agree that your contributions will be licensed under the
MIT License.
