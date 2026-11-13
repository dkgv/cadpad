# Cadpad

Desktop app for building and inspecting 3D models with AI. You describe a part or a change, Cadpad builds it as a [CadQuery](https://cadquery.readthedocs.io/) script, and you look at the result, adjust it, and export STEP or STL.

CadQuery is the modeling API. Its geometry kernel is [OpenCASCADE](https://dev.opencascade.org/).

Work in progress. Developed for macOS.

## Use it

### Projects

A project is a folder of Python source, reference files, and Cadpad settings.

- **File → New Project** (`Cmd+N`) starts one.
- **File → Open…** (`Cmd+O`) opens a folder, including an existing CadQuery repo.
- **File → Save** (`Cmd+S`) writes back to that folder. **Save As…** (`Cmd+Shift+S`) copies it elsewhere.

**Explorer** lists models it found in the source, grouped by folder. Click one to build it and open a tab. `Cmd+Shift+O` jumps to a model in the current project. **Files**, collapsed by default, shows the Python and a source preview.

Cadpad runs models with its own Python and CadQuery. It does not install the project's other dependencies. It indexes source without running it, so the list can include helpers and can miss real models. A build is what confirms that an entry produces geometry. Functions that need arguments are opened from **Files**.

Limits: 4,096 Python files, 1 MiB each, 8 MiB total. Virtual environments and Python caches are skipped.

### Assistant

**Cadpad → Settings…** (`Cmd+,`). Use a local CLI (Claude Code, Codex, or Cursor) or an Anthropic API key. CLI keys stay with that tool; an API key is stored in the system keychain.

Open the **Assistant** panel and describe the model or the change. The assistant edits the project and rebuilds.

**External agents** in Settings shows the MCP config for connecting another agent. The desktop app has to be running for that bridge.

You can also open and build projects with no assistant.

### Viewport

Click a face, edge, or object to inspect it. **Model → Select** switches what a click picks. **View → Show Dimensions** (`Cmd+Shift+M`) shows measurements.

Camera: **Fit Model** (`Cmd+0`), isometric (`Cmd+1`), front (`Cmd+2`), top (`Cmd+3`), right (`Cmd+4`). **Model → Rebuild Model** (`Cmd+R`) runs the current model again.

**Move** drives joints. **Physics** is an approximate preview of forces and contact; the assembly needs collision shapes first. **File → Export** writes STEP or STL.

## Development

You need Rust, Node.js and npm, [uv](https://docs.astral.sh/uv/), and the Xcode Command Line Tools.

```sh
uv sync --locked --python 3.12
npm ci
npm run desktop
```

The first build compiles the Rust crates and the Tauri app. It takes a while.

| Path | What it is |
| --- | --- |
| `apps/desktop` | Tauri shell and React UI |
| `packages/viewer` | 3D viewer |
| `crates/core` | Projects, builds, geometry, motion |
| `crates/agent` | Built-in assistant |
| `crates/mcp` | MCP server for external agents |
| `python/cadpad_worker` | CadQuery worker |

Checks, from the repo root:

```sh
.venv/bin/python -m pytest -q
cargo test --locked -p cadpad-core -p cadpad-agent -p cadpad-mcp
cargo clippy --locked -p cadpad-core -p cadpad-agent -p cadpad-mcp --all-targets -- -D warnings
npm run build
npm run test:viewer
npm run test:ui
cargo check --locked -p cadpad-desktop
```
