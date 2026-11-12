# Cadpad

Cadpad is a desktop app for building and exploring 3D models with AI. Describe what you want to make, inspect the result, adjust it, and export it as STEP or STL.

Cadpad is a work in progress, developed primarily for macOS.

## Usage

Try the rail and carriage example to explore the workspace.

- Configure the built-in assistant under **Cadpad → Settings…**, then describe a model or change in the assistant panel. You can also connect an external agent through MCP using the configuration under **External agents**.
- Press **Cmd+O** to open a project folder. Save your work to the same folder or use **Save as** to create another. Press **Cmd+Shift+O** to jump to a model from the current project.
- Select parts, faces, or edges to inspect them. Adjust published parameters and turn on **Dimensions** to see measurements.
- Use **Move** to explore models with joints, and **Physics** to experiment with simulation.
- Export your model as STEP or STL.

Projects can also be opened and built without an assistant. Simulation and some interactions still need further validation.

Open an existing CadQuery repository with **Cmd+O**. The folder opens directly
into its project library. **Explorer → Models** lists detected geometry entries grouped by folder.
The separate **Files** section shows Python source files and opens a source preview.
Models is the primary view; Files starts collapsed. Built geometry for the active
tab appears at the top of Models. Automatically adapted compounds expose their topology
children as separate selectable geometry; this does not infer physical parts. Candidates are
inferred without executing project code. They can include helpers, detection can
miss entries, and a candidate is confirmed to produce geometry only when built.
Names and source patterns do not establish that something is a physical part or
an assembly.

Selecting an entry opens a named model tab within the current project. Clicking
an already-open entry focuses its tab. Each model tab retains its own build and
selection; opening another project replaces the project and its tabs. Opening a
model tab does not write generated view adapters into the project folder. Selecting a file
uses its recognized factory when available, or builds its detected entries together.
This preserves the source layout and skips guarded main export routines. Functions
requiring arguments remain accessible through Files. Project-specific dependencies
are not installed automatically.

Cadpad uses these terms:

| Term | Meaning |
| --- | --- |
| Project | The folder containing source, reference files, and Cadpad settings. |
| Source entry | A function or value identified in source; a geometry candidate may produce geometry when built. |
| Build | Execute an entry to evaluate its geometry. |
| Model | The geometry produced by a build and shown in the viewport. |
| Object | One selectable output in the model; it can contain several solids. |
| Mode | An interaction state, such as selection, movement, or simulation. |

Part and assembly describe design intent or explicitly provided structure; the
source index does not prove either. The interface uses generic entry and object
labels for inferred source and evaluated output respectively.

Projects support up to 4,096 Python files (1 MiB per file, 8 MiB total); virtual
environments and Python caches are excluded. Models run with Cadpad's bundled
Python dependencies, so additional project dependencies are not installed automatically.

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
