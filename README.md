# Cadpad

Cadpad is a desktop app for building and exploring 3D models with AI. Describe what you want to make, inspect the result, adjust it, and export it as STEP or STL.

Cadpad is a work in progress, developed primarily for macOS.

## Usage

Try the rail and carriage example to explore the workspace.

- Configure the built-in assistant under **Cadpad → Settings…**, then describe a model or change in the assistant panel. You can also connect an external agent through MCP using the configuration under **External agents**.
- Press **Cmd+O** to open a project folder. Save your work to the same folder or use **Save as** to create another.
- Select parts, faces, or edges to inspect them. Adjust published parameters and turn on **Dimensions** to see measurements.
- Use **Move** to explore models with joints, and **Physics** to experiment with simulation.
- Export your model as STEP or STL.

Projects can also be opened and built without an assistant. Simulation and some interactions still need further validation.

## Development

You'll need Rust, Node.js and npm, [uv](https://docs.astral.sh/uv/), and the Xcode Command Line Tools on macOS.

```sh
uv sync --locked --python 3.12
npm ci
npm run desktop
```

The first build can take a while.

Run the relevant checks from the project directory:

```sh
.venv/bin/python -m pytest -q
cargo test --locked -p cadpad-core -p cadpad-agent -p cadpad-mcp
cargo clippy --locked -p cadpad-core -p cadpad-agent -p cadpad-mcp --all-targets -- -D warnings
npm run build
npm run test:viewer
npm run test:ui
cargo check --locked -p cadpad-desktop
```
