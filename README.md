# Vela

**A minimal, safe systems programming language that compiles to portable C11.**

[Online Documentation](https://pya-h.github.io/vela/) · [Language Reference](https://pya-h.github.io/vela/) · [Regions Guide](https://pya-h.github.io/vela/regions.html) · [Channels & Ownership](https://pya-h.github.io/vela/channels.html) · [GUI & 2D Rasterizer](https://pya-h.github.io/vela/gui.html) · [Standard Library](https://pya-h.github.io/vela/std.html) · [Web Architecture](https://pya-h.github.io/vela/web.html) · [Vela Studio](https://pya-h.github.io/vela/ide.html)

---

## Overview

Vela is a statically typed systems programming language designed for clarity, deterministic resource control, and developer ergonomics. The compiler is written in dependency-free Rust and translates Vela code into clean, standard ISO C11 that compiles with any C compiler (`gcc`, `clang`, MSVC).

Vela avoids the overhead of garbage collection runtimes, reference-counting cycles, and complex lifetime annotations by organizing dynamic memory into lexical **regions**. Memory allocated in a region is reclaimed all at once when its scope exits. Pointers cannot outlive their enclosing region and cannot be null.

### Key Pillars

- **Zero-GC Region Memory:** Allocate dynamically with `new T at r` or inside a `region r default` block. Bulk deallocation on block exit eliminates leaks and frees without pointer-chasing garbage collector pauses.
- **Deep Immutability & Predictable Places:** Names are `fixed` (deeply immutable by default), `bind` (immutable container with mutable contents), or `flex` (reassignable). Parameters pass by value or by explicit caller place (`flex& x: T` / `&x`) with compile-time aliasing protection preventing simultaneous mutable aliasing.
- **Explicit Failure Channels:** No runtime exceptions. Fallible tasks declare typed failure channels `-> T ~ E`, propagated cleanly with `?` or caught with plan-B expressions (`val { fallback }`).
- **Two-Mode Decorators:** Declarative startup decorators (`$get("/path")`, `$enroll`) execute before `main` to register routes and services; wrapper decorators (`$(auth)`, `$(logged)`) compose reusable middleware around tasks at call time.
- **Batteries-Included Standard Library:** Embedded in the compiler under `std/`: file system operations (`std/files`), math (`std/math`), pipes and lazy iterator chains (`std/collections`), loop control (`std/loops`), 2D software canvas rasterization, windowing, and widgets (`std/gui`), thread-safe channels with ownership transfer (`Chan[own T]`), and a non-blocking web server with WebSockets, routing, and an event loop (`std/web`).
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
- **[Standard Library](https://pya-h.github.io/vela/std.html)**: Reference for `std/web`, `std/files`, `std/collections`, `std/math`, `std/loops`, and `std/gui`.
- **[Channels & Ownership Transfer](https://pya-h.github.io/vela/channels.html)**: Bounded channels (`Chan[T]`, `Chan[own T]`), Go-style concurrency, and race-free linear ownership transfer across threads.
- **[GUI & 2D Rasterizer](https://pya-h.github.io/vela/gui.html)**: 2D software canvas rasterizer, desktop window presentation (X11), rich interactive widgets (Button, Label, TextBox, ListBox, CheckBox, ProgressBar, ImageView, MessageBox, InputBox), styling presets, and headless automated testing.
- **[Web Architecture](https://pya-h.github.io/vela/web.html)**: Single event loop, non-blocking descriptor polling (`Poll`), persistent keep-alive, read/idle timeouts, RFC 6455 WebSockets, and middleware design.
- **[Vela Studio IDE](https://pya-h.github.io/vela/ide.html)**: Browser-based WebAssembly development environment with live diagnostics and terminal.
- **[Language Comparison](https://pya-h.github.io/vela/comparison.html)**: Architectural and philosophical comparison of Vela with Rust, Go, C, Zig, and Python.
- **[Safety & Invariants](https://pya-h.github.io/vela/safety.html)**: Static verification, bounds checking, overflow protection, and runtime traps.
- **[Project Review](https://pya-h.github.io/vela/review.html)**: Design critique, strengths, and evolution.
- **[Future Notebook](https://pya-h.github.io/vela/future.html)** and **[Notebook 2](https://pya-h.github.io/vela/future-2.html)**: Explored language extensions and design experiments.
- **[Events](https://pya-h.github.io/vela/events.html)**, **[Tricks](https://pya-h.github.io/vela/tricks.html)**, and **[May Waste](https://pya-h.github.io/vela/maywaste.html)**.

Offline documentation sources are also kept directly in the [`docs/`](docs/) directory of this repository.

---

## Requirements

- **Rust 1.88 or later**
- **A standard C compiler:** `cc`, `gcc`, `clang`, or specified via `$CC` (e.g. `CC="gcc -O3"`)
- **For Vela Studio only:** The `wasm32-unknown-unknown` Rust target and **Node.js 22.12+**

---

## Build and Run

### Building the Compiler

```sh
cargo build --release
```

### Compiling and Running a Program

```sh
# Compile and run an example
./target/release/vela examples/hello.vela -o hello
./hello

# Inspect generated C11 code
./target/release/vela examples/hello.vela --emit-c
```

### Compiler CLI Flags

```
vela <input.vela> [-o <output>] [--emit-c | --check | --quick] [--no-notices] [--repeat-warnings] [--]
```

- `-o <path>`: Names the generated executable (`a.out` by default).
- `--emit-c`: Outputs the generated ISO C11 code to stdout or to `-o`.
- `--check`: Verifies syntax and type correctness without generating C or invoking the C compiler.
- `--quick`: Builds with `-O0` for faster compilation times during development.
- `--no-notices`: Suppresses runtime informational notices on stderr.
- `--repeat-warnings`: Shows every repeated warning occurrence across tasks and files.

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

## Vela Studio (IDE)

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
| `ide/` | Vela Studio crates: `analysis` (LSP engine), `wasm` (browser bridge), `server` (backend daemon), and `web` (frontend) |

---

## License

Vela is developed by pya-h. See [COPYRIGHT.md](COPYRIGHT.md) for copyright and attribution information.
