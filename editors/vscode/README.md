# Vela Language Support for Visual Studio Code

Official Visual Studio Code extension for **Vela**, providing fast syntax highlighting, rich language diagnostics, autocompletion, type hovers, go-to-definition, and symbol navigation powered by the native `vela-lsp` Language Server Protocol daemon.

---

## Features

- **Instant Syntax Highlighting**: Declarative TextMate grammar colorizes keywords, operators (`<<-`, `->>`, `^`, `@`, `~`), comments (`;`), strings with format interpolations (`{...}`), types, and decorators.
- **Smart Indentation**: Auto-indentation for blocks (`task`, `when`, `select`, `loop`, `match`, `kind`, `shape`, `actor`) and smart outdent on `fi`, `case`, `else`, `otherwise`.
- **Live Diagnostics**: Real-time error and warning squiggles as you type, mapped to exact line and column numbers.
- **IntelliSense & Autocompletion**: Context-aware completions for keywords, local variables, task definitions, shapes, methods, fields, and common code snippets.
- **Hover & Documentation**: Inspect type signatures and doc comments on any identifier.
- **Go to Definition**: Jump directly to task, kind, shape, or variable declarations across workspace files.
- **Document Outline / Symbols**: Quick symbol navigation via `Ctrl+Shift+O` (`Cmd+Shift+O` on macOS).
- **Emitted C Inspection**: Inspect the generated ISO C11 code side-by-side with the `Vela: Show Emitted C11 Code` command.

---

## Prerequisites

The language intelligence features (diagnostics, completion, hover, definition) are powered by the native `vela-lsp` daemon.

Install `vela-lsp` from the Vela repository:

```sh
# Build and install to ~/.cargo/bin
cargo install --path ide/lsp --force
```

Ensure `~/.cargo/bin` is in your `PATH`.

---

## Installation

### Manual Installation (Development / Local)

1. Copy or symlink this directory into your VS Code extensions folder:
   - **Linux / macOS**: `~/.vscode/extensions/vela`
   - **Windows**: `%USERPROFILE%\.vscode\extensions\vela`

```sh
# From the repository root:
ln -s "$(pwd)/editors/vscode" ~/.vscode/extensions/vela
```

2. Reload Visual Studio Code (`Developer: Reload Window`).

### Packaging as `.vsix`

To build a standalone installable package:

```sh
cd editors/vscode
npx @vscode/vsce package
```
Then install in VS Code via `Extensions: Install from VSIX...`.

---

## Configuration

| Setting | Default | Description |
|---|---|---|
| `vela.serverPath` | `""` | Custom path to the `vela-lsp` or `vela` executable. If empty, the extension searches `PATH` and `~/.cargo/bin`. |
| `vela.trace.server` | `"off"` | Traces JSON-RPC communication between VS Code and `vela-lsp` (`"off"`, `"messages"`, `"verbose"`). |

---

## Commands

- `Vela: Restart Language Server`: Restarts the underlying `vela-lsp` background process.
- `Vela: Show Emitted C11 Code`: Lowers the active Vela file to standard ISO C11 and opens it in a split editor.
