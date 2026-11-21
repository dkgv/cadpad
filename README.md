# Cadpad

A macOS app for building 3D models with AI. Describe a part, inspect it, ask for changes, and export STEP or STL. Models are [CadQuery](https://cadquery.readthedocs.io/) Python files you can edit yourself.

Still a work in progress.

## Run it

You'll need Rust, Node.js and npm, [uv](https://docs.astral.sh/uv/), and the Xcode Command Line Tools.

```sh
uv sync --locked --python 3.12
npm ci
npm run desktop
```

The first build takes a while.

## Use it

Create a project (`Cmd+N`) or open an existing CadQuery folder (`Cmd+O`). Choose an LLM backend in **Settings** (`Cmd+,`), then open the **Assistant** and describe what you want to make.

Click a model in the **Explorer** to build it. Click faces and edges to inspect them, use `Cmd+0` to fit the view, and `Cmd+R` to rebuild. Save with `Cmd+S` and export through **File → Export**.

You can work without an assistant, too. To connect an external agent, copy the MCP config from **Settings → External agents** and keep Cadpad running.

Cadpad uses its own Python environment and doesn't install your project's extra dependencies. Model discovery can miss entries or include helpers; building one confirms whether it produces geometry.

## LLM support

The built-in Assistant supports these backends:

| Backend | Authentication | Model selection |
| --- | --- | --- |
| Claude Code CLI (`claude`) | Sign in with `claude auth login` | Aliases discovered from CLI help, with fallback aliases; custom IDs supported |
| Codex CLI (`codex`) | Sign in with `codex login` | Catalog from `codex debug models`; custom IDs supported |
| Cursor CLI (`cursor-agent` or `agent`) | Sign in with `agent login` | Catalog from `--list-models`; custom IDs supported |
| OpenCode CLI (`opencode`, version 2+) | Existing OpenCode provider configuration and accounts; use `opencode auth login` to connect a provider | Catalog from `opencode models`; custom `provider/model#variant` IDs supported |
| Anthropic API | API key saved in the system keychain | Catalog from the Anthropic Models API; an explicit model ID is required |

Install and authenticate a CLI before selecting it in Settings. Cadpad must be able to find its executable on `PATH`. CLI backends use the CLI's existing account and configuration; Cadpad supplies its modeling tools through MCP. OpenCode uses global configuration and runs a private server for each turn. Providers and local models configured in OpenCode are usable when OpenCode exposes them; the selected model must support the tool calls needed for modeling. Direct API integration is currently limited to Anthropic.

For CLIs, **Provider default** leaves model choice to the harness. A selected model is passed to new turns with `--model`. **Custom model ID** remains available when catalog discovery fails or misses a model. Discovery does not guarantee that your account can use every listed model, and Claude's aliases are not an account-specific catalog. Settings currently saves one backend/model pair, rather than remembering a separate choice for each harness.

All backends can read project source, stage edits, build geometry, inspect results, and export through Cadpad's tools. The Assistant displays text, reasoning, and tool activity when the backend reports them, and supports stopping a turn. OpenCode reports tool activity after each tool finishes. CLI turns start fresh harness sessions with recent conversation text supplied as context. Selection references are passed as text; automatic reference screenshot attachments currently go to the Anthropic API backend only. CLI agents can request viewport captures through MCP, subject to the harness and model's image support.

Agents can use `capture_views` for up to four labeled angles (isometric, front, right, and top by default). Set `single=true` for a standalone screenshot using the current camera, a named preset, or an explicit camera. For a before/after comparison, capture before editing, then pass the returned `capture_id` to `compare_views` after building. Comparisons reuse the original cameras and return paired images with revision and pose context. Captures require the open desktop viewport and are temporary: the last eight sets are retained in memory, scoped to the originating tool session. Built-in Assistant sessions end with each turn, so capture and compare within the same request. Screenshots support visual checks; use exact inspection for measurements.

Other agents can connect through **Settings → External agents** using the supplied MCP configuration. This exposes Cadpad's tools to an MCP-compatible client without adding that client's harness to the built-in Assistant.

## Development

The app is Tauri + React, with a Rust core and a Python CadQuery worker.

- `apps/desktop` — desktop app
- `packages/viewer` — 3D viewer
- `crates/core`, `crates/agent`, `crates/mcp` — core, assistant, and MCP server
- `python/cadpad_worker` — modeling worker

Checks from the repo root:

```sh
.venv/bin/python -m pytest -q
cargo test --locked -p cadpad-core -p cadpad-agent -p cadpad-mcp
cargo clippy --locked -p cadpad-core -p cadpad-agent -p cadpad-mcp --all-targets -- -D warnings
npm run build
npm run test:viewer
npm run test:ui
cargo check --locked -p cadpad-desktop
```
