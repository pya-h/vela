# Vela

**A minimal, safe systems programming language that compiles to portable C11.**

[Online Documentation](https://pya-h.github.io/vela/) · [Language Reference](https://pya-h.github.io/vela/) · [Regions Guide](https://pya-h.github.io/vela/regions.html) · [Channels & Ownership](https://pya-h.github.io/vela/channels.html) · [Actors](https://pya-h.github.io/vela/actors.html) · [Events](https://pya-h.github.io/vela/events.html) · [Commitments](https://pya-h.github.io/vela/commitment.html) · [GUI & 2D Rasterizer](https://pya-h.github.io/vela/gui.html) · [Standard Library](https://pya-h.github.io/vela/std.html) · [Web Architecture](https://pya-h.github.io/vela/web.html) · [Benchmarks](https://pya-h.github.io/vela/benchmarks.html) · [Vela Studio](https://pya-h.github.io/vela/ide.html)

---

## Overview

Vela is a statically typed systems programming language designed for clarity, deterministic resource control, and developer ergonomics. The compiler is written in dependency-free Rust and translates Vela code into clean, standard ISO C11 that compiles with any C compiler (`gcc`, `clang`, MSVC).

Vela avoids the overhead of garbage collection runtimes, reference-counting cycles, and complex lifetime annotations by organizing dynamic memory into lexical **regions**. Memory allocated in a region is reclaimed all at once when its scope exits. Pointers cannot outlive their enclosing region and cannot be null.

### Key Pillars

- **Zero-GC Region Memory:** Allocate dynamically with `new T at r` or inside a `region r default` block. Bulk deallocation on block exit eliminates leaks and frees without pointer-chasing garbage collector pauses.
- **Deep Immutability & Predictable Places:** Names are `fixed` (deeply immutable by default), `bind` (immutable container with mutable contents), or `flex` (reassignable). Parameters pass by value or by explicit caller place (`flex& x: T` / `&x`) with compile-time aliasing protection preventing simultaneous mutable aliasing.
- **Explicit Failure Channels:** No runtime exceptions. Fallible tasks declare typed failure channels `-> T ~ E`, propagated cleanly with `?` or caught with plan-B expressions (`val { fallback }`).
- **Two-Mode Decorators:** Declarative startup decorators (`$get("/path")`, `$enroll`) execute before `main` to register routes and services; wrapper decorators (`$(auth)`, `$(logged)`) compose reusable middleware around tasks at call time.
- **Concurrency, Channels, Actors & `select`:** Bounded FIFO channels (`Chan[T]`, `Chan[own T]`), shorthand arrow operators (`val ->> ch`, `<<- ch`), multi-channel `select ... case ... otherwise ... fi` multiplexing with optional `after ms` timeouts, and share-nothing `actor` tasks scheduled across an M:N worker pool with compile-time linear move safety.
- **Reactive Events & Alters Contracts:** Declarative `event` hooks (`on`, `on read`, `on init`, `on drop`, `on change`, `on start`, `on end`, `before`/`after`) verified at compile time with transitive `alters(...)` contracts, `auto`/`.listen()` control, and terminating cycle checks.
- **Batteries-Included Standard Library:** Embedded in the compiler under `std/`: collections and lazy iterators (`std/collections`), file system (`std/files`), 2D canvas/X11 GUI (`std/gui`), 512-bit checked integers (`std/int512`), strict JSON (`std/json`), loop results (`std/loops`), math (`std/math`), sync channels (`std/sync`), and non-blocking HTTP/1.1, SSE, and WebSockets (`std/web`).
- **Vela Studio IDE:** A self-contained WebAssembly-powered web IDE (`ide/`) with live diagnostics, syntax highlighting, completions, and integrated terminal sessions.

---

## A Taste of Vela

### REST API with Routing and Middleware (`std/web`)

```vela
include std/web { get, post, serve, Request, Response, WebError, method_name }

; Middleware: logs incoming requests before passing control to the route handler.
task logged(next: (Request@r, @r) -> Response@r ~ WebError@r, req: Request@r, r: region default) -> Response@r ~ WebError@r
    show("[", method_name(req.method), "] ", req.path)
    <- next(req, r)
fi

; Startup decorator registers this endpoint to the default router before main.
$get("/users/\{id\}")
$(logged)
task get_user(req: Request@r, r: region default) -> Response@r ~ WebError@r
    fixed id = req.param("id") { "anonymous" }
    <- Response.text("User profile: {id}\n" at r, r)
fi

task main()
    show("Listening on http://127.0.0.1:8080 ...")
    serve("127.0.0.1:8080") { failure err
        show("server stopped: ", err)
    }
fi
```

### Region-Based Data Structures

```vela
kind Tree
    Leaf(value: int)
    Branch(left: ^Tree, right: ^Tree)
fi

task build_tree(depth: int, seed: int, r: region) -> Tree@r
    when depth == 0
        <- Leaf(seed % 10)
    fi
    fixed left = new Tree(build_tree(depth - 1, seed * 3 + 1) at r) at r
    fixed right = new Tree(build_tree(depth - 1, seed * 7 + 2) at r) at r
    <- Branch(left, right)
fi

task sum_tree(t: Tree@r) -> int
    <- match t
    case Leaf value = value
    case Branch left right = sum_tree(left^) + sum_tree(right^)
    fi
fi

task main()
    region scratch default
    fixed tree = build_tree(4, 42, scratch)
    show("Tree sum: ", sum_tree(tree))
fi
```

---

## Language Highlights

### 1. Region-Based Memory Management
Dynamic allocations live in named or default regions:
```vela
region work default
fixed ptr = new Point { x: 10, y: 20 }
show(ptr^.x)
; The entire region 'work' is reclaimed cleanly here when the block exits.
```
Pointers (`^T@r`) track their owning region at compile time, guaranteeing that pointers never outlive their memory and cannot be null.

### 2. Mutability and Places
- `fixed x = 1`: Deeply immutable. Nothing changes through it.
- `bind p = Point { x: 1, y: 2 }`: Bound to one identity. Fields can mutate, but the binding itself cannot be reassigned.
- `flex count = 0`: Fully mutable place.
- `flex& total: int`: Takes the caller's place by reference (passed as `&total`). The compiler enforces that no place is passed twice where it may change, preventing data races and aliasing bugs.

### 3. Explicit Failure Channels and Plan-B
Tasks return values or explicit failures using the `~` operator:
```vela
task parse_port(text_val: text@p) -> int ~ WebError
    fixed n = int(text_val) { failure _
        <- fail WebError.BadRequest("invalid port number")
    }
    <- n
fi

; Fallback with plan B:
fixed port = parse_port("8080") { 8080 }
```

### 4. Method-Aware Routing and Startup Decorators
Decorators beginning with `$` run at startup to enroll routes into the default router:
```vela
$get("/items/\{id\}")
$post("/items")
$delete("/items/\{id\}")
```
Patterns capture dynamic segments with `{param}` or wildcards with `*`. Unmatched paths yield HTTP 404 (`RouteNotFound`), while method mismatches yield HTTP 405 (`MethodNotAllowed`).

---

## Documentation

Full guides, tutorials, and architectural specifications are available online at **[https://pya-h.github.io/vela/](https://pya-h.github.io/vela/)** (deployed from the `docs` branch):

- **[Language Reference](https://pya-h.github.io/vela/)** (`index.html`): Complete guide to grammar, tasks, shapes, kinds, expressions, loops, and generics.
- **[Regions & Memory](https://pya-h.github.io/vela/regions.html)**: Scoped region memory, pointer safety (`^T@r`), default regions, and zero-GC lifecycle.
- **[Standard Library](https://pya-h.github.io/vela/std.html)**: Reference for `std/collections`, `std/files`, `std/gui`, `std/int512`, `std/json`, `std/loops`, `std/math`, `std/sync`, and `std/web`.
- **[Channels & Ownership Transfer](https://pya-h.github.io/vela/channels.html)** and **[Channel Lifecycle](https://pya-h.github.io/vela/channel-lifecycle.html)**: Bounded channels (`Chan[T]`, `Chan[own T]`), `<<-` / `->>` shortcuts, multi-channel `select`, explicit `.close()` vs. freeing semantics, and race-free linear ownership transfer across threads.
- **[Actors & M:N Scheduler](https://pya-h.github.io/vela/actors.html)**: Encapsulated stateful `actor` declarations, mailboxes, `spawn`, and lock-free message handling.
- **[Events & Reactive Triggers](https://pya-h.github.io/vela/events.html)**: Compile-time triggers (`on`, `on read`, `before`/`after`, lifecycle hooks), `.listen()`/`auto`, and `alters` causality checks.
- **[Concurrency & Design Boundaries](https://pya-h.github.io/vela/concurrency.html)** and **[Commitments Design](https://pya-h.github.io/vela/commitment.html)**: Current concurrency guarantees, architectural limits, and await-by-default async design exploration.
- **[GUI & 2D Rasterizer](https://pya-h.github.io/vela/gui.html)**: 2D software canvas rasterizer, desktop window presentation (X11), rich interactive widgets (Button, Label, TextBox, ListBox, CheckBox, ProgressBar, ImageView, MessageBox, InputBox), styling presets, and headless automated testing.
- **[Web Architecture](https://pya-h.github.io/vela/web.html)**: Single event loop, non-blocking descriptor polling (`Poll`), persistent keep-alive, read/idle timeouts, RFC 6455 WebSockets, and middleware design.
- **[Vela Studio IDE](https://pya-h.github.io/vela/ide.html)**: Browser-based WebAssembly development environment with live diagnostics and terminal.
- **[Language Comparison](https://pya-h.github.io/vela/comparison.html)**: Architectural and philosophical comparison of Vela with Rust, Go, C, Zig, and Python.
- **[Benchmarks & Engineering Roadmap](https://pya-h.github.io/vela/benchmarks.html)**: Cross-language performance benchmarks against C, Rust, Go, and Zig across 9 workloads, root-cause forensics, and an interactive optimization simulator.
- **[Safety, Data Races & Invariants](https://pya-h.github.io/vela/safety.html)**: Static verification, bounds checking, overflow protection, data-race defense layers, and runtime traps.
- **[Project Review](https://pya-h.github.io/vela/review.html)**: Design critique, strengths, and evolution.
- **[Future Directions](https://pya-h.github.io/vela/future.html)**: Consolidated roadmap, open design questions, and future compiler/stdlib directions.
- **[Tricks](https://pya-h.github.io/vela/tricks.html)** and **[May Waste](https://pya-h.github.io/vela/maywaste.html)**: Type-level filtering tricks and language-surface audits.

Offline documentation sources are also kept directly in the [`docs/`](docs/) directory of this repository.

---

## Requirements

- **Rust 1.88 or later**
- **A standard C compiler:** `cc`, `gcc`, `clang`, or specified via `$CC` (e.g. `CC="gcc -O3"`)
- **For Vela Studio only:** The `wasm32-unknown-unknown` Rust target and **Node.js 22.12+**

---

## Build and Run: Command-Line Compiler (`vela`)

Vela includes a standalone command-line compiler driver (`vela`) that compiles Vela source files and multi-file projects into native executables, behaving just like `gcc` or `g++`.

### 1. Building and Installing the Compiler

#### Option A: Build the release binary locally
```sh
cargo build --release
```
The optimized binary will be created at `./target/release/vela`.

#### Option B: Install globally to `$PATH` (Recommended)
To run `vela` from any terminal or directory on your system:
```sh
cargo install --path .
```
This places `vela` into `~/.cargo/bin/vela`. Ensure `~/.cargo/bin` is in your `PATH` environment variable.

> [!TIP]
> **Updating to Future Versions**:
> Whenever you pull new changes or update the compiler codebase, you can reinstall and overwrite the existing binary by running:
> ```sh
> cargo install --path . --force
> ```
> To remove the binary at any time, run `cargo uninstall vela`.

---

### 2. Compiling and Running Programs (Like `gcc` / `g++`)

#### Default Compilation (Outputs `a.out`)
Just like running `gcc main.c`:
```sh
vela examples/basics/hello.vela
./a.out
```

#### Specify Output Binary with `-o`
Just like running `gcc main.c -o myapp`:
```sh
vela examples/basics/hello.vela -o hello
./hello
```

#### Syntax and Type Checking (`--check`)
Quickly verify syntax, types, and region lifetimes without generating C code or invoking the C compiler:
```sh
vela program.vela --check
```

#### Inspect Generated C11 Code (`--emit-c`)
Output clean, standard ISO C11 code to stdout or to a file:
```sh
vela program.vela --emit-c
vela program.vela --emit-c -o generated.c
```

#### Fast Compilation During Development (`--quick`)
Build using the host C compiler with `-O0` to skip heavy optimization passes for faster turnaround:
```sh
vela program.vela -o dev_build --quick
```

#### Custom C Compiler and Flags
By default, Vela invokes the host `cc` compiler. You can customize the compiler binary and optimization flags:
```sh
# Via CLI flag:
vela program.vela -o myapp --cc "gcc -O3"

# Or via environment variable:
CC="clang -O3 -march=native" vela program.vela -o myapp
```

---

### 3. Compiler CLI Reference

```
vela <input.vela> [-o <output>] [--emit-c | --check | --quick] [--no-notices] [--repeat-warnings] [--cc <cmd>] [--]
vela --help | --version
```

| Option | Description |
|---|---|
| `<input.vela>` | Source file to compile (automatically resolves all `include` modules). |
| `-o <path>` | Sets output binary executable path (`a.out` by default). |
| `--emit-c` | Emits generated ISO C11 code instead of compiling an executable. |
| `--check` | Runs front-end checks (parser, type checker, regions) without code generation. |
| `--quick` | Compiles C code with `-O0` for fast compilation cycles. |
| `--no-notices` | Suppresses informational diagnostics on stderr. |
| `--repeat-warnings` | Displays repeated warning occurrences across tasks and files. |
| `--cc <cmd>` | Specifies custom C compiler command and flags (defaults to `$CC` or `cc`). |
| `--help` | Displays usage summary. |
| `--version` | Displays compiler version. |

---

### 4. Compilation Pipeline Under the Hood

When you execute `vela input.vela -o app`:
1. **Frontend**: The parser and resolver process the entry point and transitively load all `include` modules and standard library components from `std/`.
2. **Analysis**: Static type checking, region lifetime verification, place mutability checks, linear ownership enforcement, and event alters-causality checks are performed.
3. **C11 Code Generator**: The validated AST is lowered into portable, dependency-free ISO C11 code.
4. **Native Backend**: Vela invokes the host C compiler (`cc`/`gcc`/`clang`) linked against the embedded minimal runtime header (`runtime/vela_rt.h`), producing a fast, self-contained native binary.

---

## Running Tests

```sh
# Run compiler unit and integration tests
cargo test --workspace --release --no-fail-fast

# Strict ISO C11 runtime tests under AddressSanitizer and UndefinedBehaviorSanitizer
gcc -std=c11 -pedantic-errors -Wall -Wextra -Werror -fsanitize=address,undefined -I runtime tests/runtime/sockets.c -o /tmp/sockets_test && /tmp/sockets_test

# Linter checks
cargo clippy --workspace --all-targets -- -D warnings
```

---

## IDE & Editor Support

### Visual Studio Code Extension
Vela provides a native VS Code extension under [`editors/vscode/`](editors/vscode/) with syntax highlighting, smart indentation, live diagnostics, autocompletion, type hovers, and symbol navigation powered by the native `vela-lsp` daemon:

```sh
# 1. Install the native LSP daemon
cargo install --path ide/lsp --force

# 2. Link the extension into VS Code
ln -s "$(pwd)/editors/vscode" ~/.vscode/extensions/vela
```

### Vela Studio (Browser IDE)

Vela Studio provides a browser-based IDE powered by the compiler's analysis engine compiled to WebAssembly:

```sh
# Install WebAssembly target
rustup target add wasm32-unknown-unknown

# Build web client
npm --prefix ide/web install
npm --prefix ide/web run build

# Start Vela Studio workspace server
cargo run -p vela-studio --release -- examples
```

See [docs/ide.html](https://pya-h.github.io/vela/ide.html) for shortcuts, live diagnostic features, and security settings.

---

## Repository Structure

| Directory | Purpose |
|---|---|
| `src/` | Vela compiler in Rust: lexer, parser, type checker, C11 emitter, and CLI driver |
| `runtime/vela_rt.h` | Minimal runtime header included by generated C, containing native socket and I/O implementations |
| `std/` | Vela standard library written in Vela and embedded into the compiler binary (`std/web`, `std/files`, etc.) |
| `examples/` | Example Vela programs demonstrating features and paradigms |
| `tests/` | Comprehensive test suite: `tests/run/` (execution tests), `tests/fail/` (typecheck rejections), and `tests/runtime/` (C11 sanitizers) |
| `docs/` | Documentation source files, reference guides, and design specifications |
| `editors/` | Editor extensions, including the official Visual Studio Code extension (`editors/vscode/`) |
| `ide/` | Tooling and IDE crates: `analysis` (engine), `lsp` (`vela-lsp` daemon), `wasm`, `server`, and `web` |

---

## License

Vela is developed by pya-h. See [COPYRIGHT.md](COPYRIGHT.md) for copyright and attribution information.
