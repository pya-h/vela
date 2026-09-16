# Vela

Vela is a small, statically typed systems language that compiles to portable C11. It has one kind of
function (`task`), one kind of record (`shape`), one kind of sum type (`kind`), exhaustive `match`,
immutable-by-default bindings, and memory in regions: `new T(v) at r` allocates in a region that is freed
when its block closes, and pointer types (`^T@r`) keep every pointer from outliving it or being null. A value that may
be missing is a `maybe T`, and a task that can fail sends `T or E`; both are read with `match`, `is`, or a
plan B (`x { fallback }`). Blocks close with `fi`, and a task sends its results with `<-`: one value, or
several (`-> int, int` … `<- a / b, a % b`), received with `bind q, r = divmod(17, 5)`.

```
task fib(n: int) -> int
    when n < 2
        <- n
    else
        <- fib(n - 1) + fib(n - 2)
    fi
fi

task main()
    each i in 0..10
        show(fib(i))
    fi
fi
```

The compiler is written in Rust and has no dependencies. **Vela Studio**, an editor for Vela with live
errors, completion, one-key run, and a terminal, lives in `ide/`.

## Requirements

- Rust 1.88 or later
- A C compiler: `cc`, or the one named by `$CC` (which may include arguments, as in `CC="ccache gcc"`)
- For Vela Studio only: the `wasm32-unknown-unknown` Rust target and Node.js 22.12 or later

## Build and run

```sh
cargo build
./target/debug/vela examples/hello.vela -o examples/hello
./examples/hello
```

`vela <input.vela> [-o <output>] [--emit-c | --check]`: `-o` names the executable (`a.out` by default),
`--emit-c` prints the generated C (or writes it to the `-o` file), `--check` only checks the program
(including that it has a `main`), and `-h`/`--help` and `--version` print the usage and the version. The exit status is 0 on success, 1
when the program can't be read or doesn't compile, and 2 for wrong arguments.

## Tests

```sh
cargo test                              # the compiler
CC=clang cargo test                     # with another C compiler
cargo test --workspace                  # the compiler and Vela Studio's crates
cargo clippy --workspace --all-targets
```

Programs in `tests/run/` must print their `.out` files, programs in `tests/fail/` must be rejected with
the error in their first line, and every file in `examples/` is checked against `tests/examples/`.

## Vela Studio

```sh
rustup target add wasm32-unknown-unknown
npm --prefix ide/web install
npm --prefix ide/web run build
cargo run -p vela-studio --release -- examples
```

See [docs/ide.html](docs/ide.html) for features, shortcuts, and security notes.

## Layout

| Path | What |
|---|---|
| `src/` | The compiler: lexer, parser, checker, C generator, and the build driver |
| `runtime/vela_rt.h` | The small C runtime the generated code includes |
| `examples/` | Example programs |
| `tests/` | Program tests (`run/`, `fail/`, `examples/`) and Rust tests |
| `docs/` | The language reference, the regions guide, a comparison with other languages, design notes, and the Vela Studio guide |
| `ide/` | Vela Studio: `analysis` (language support), `wasm` (it, for the browser), `server` (`vela-studio`), `web` (the page) |

## Documentation

- [The language](docs/vela.html)
- [Regions: memory in Vela](docs/regions.html), a guide and tutorial
- [Vela Studio](docs/ide.html)
- [Vela and other languages](docs/comparison.html)
- [Future directions](docs/future.html), and a [second design notebook](docs/future-2.html)
