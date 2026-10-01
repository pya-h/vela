# Vela

Vela is a small, statically typed systems language that compiles to portable C11. It has one kind of
function (`task`), one kind of record (`shape`), one kind of sum type (`kind`), exhaustive `match`,
immutable-by-default bindings, and memory in regions: `new T(v) at r` allocates in a region that is freed
when its block closes, and pointer types (`^T@r`) keep every pointer from outliving it or being null. A block
may name one of its regions its default, `region r default` (or `r: region default` for one a task is given),
and then what allocates there needs no `at r`. Text
lasts as long as the program (`text`, as a literal does) or is in a region (`text@r`), and a shape holding
text may name one (`Person@r`). A value that may
be missing is a `maybe T`, and a task that can fail sends `T ~ E`; both are read with `match`, `is`, or a
plan B (`x { fallback }`). A `when` chains conditions with `else when`, and one `fi` closes the chain. A
`match` or a `when` can be the value itself, with every branch giving one (`bind area = match s case Circle r
= 3.14 * r * r … fi`), on a case's own line with `=` or on the last line of a block.
Blocks close with `fi`, a comment starts with `;` (`bind x = 1 ; the first`), and a task sends its results with `<-`: one value, or
several (`-> int, int` … `<- a / b, a % b`), received with `bind q, r = divmod(17, 5)`. Results can be named
(`-> total: int`), set along the way with `total <- value` and sent by reaching the end of the task. An `int`
never wraps: overflow and division by zero stop the program with a message saying where. Arrays (`[int#4]`)
are values held in place, and slices (`[int]@r`) view items in a region, made with `new [int]([0#n]) at r`
(or empty, `new [int]() at r`) and changed with `xs.add(v)`, `xs.add(i, v)`, `xs.del(i)`, and `xs.del()`;
every index is checked, and `xs.len` is the count. Text knows its length in bytes, `t.len`, has parts that
are views of the same bytes, `t[0..3]`, is ordered by `<`, and is joined into a region with `a + b at r`; `n.str() at r` (or `text(n) at r`)
writes a value as text: every value has one (`Point { x: 1, y: 2 }`), and a shape or kind may give its own with a method `str`. `float(n)` and `int(x)` turn a value into a
number the same way, by the name of the type: a float becomes its whole part toward zero, a bool 1 or 0. Text
read as a number may not be one, so `int(line)` sends `int ~ ParseError`, a builtin kind whose variants are
`NotANumber` and `TooLarge`. Text holds values written inside
it, `"{name} is {age}" at r`, and `\{` and `\}` write braces. `show` takes any number of values and writes them
one after another, then ends the line (`show("n = ", n)`, and `show()` for an empty one); any value is one it writes, `show(card)`,
with a shape's or kind's own `str` in a region it makes for the line and frees after it.
`scan("Name: ") at r` shows a prompt and reads a line into a region, giving `none` once the input has ended, and
`scan("Age: ", int)` reads a number from that line instead, needing no region and sending `int ~ InputError`,
whose variants are `Ended` and `NoNumber`, holding the `ParseError` that reading the line gave. `value.copy()` copies a value (its text, its slices'
items, and what its pointers point to) into the region where it goes, or `value.copy() at r`. Tuples hold
values together (`(text, int)`, read as `t.0`), their items never change, and names take them apart
(`bind name, age = pair`, or `when found is {(name, age)}` in a pattern). A task or a shape can take types as well as
values (`task first<T>(xs: [T]@r) -> maybe T`, `shape Box<T>`), with a word saying what may be done with them
(`fixed`, `same`, `similar`, `shown`, `ordered`, joined with `&`); the type comes from the arguments or the values given,
from the place the value goes, or written at the use (`first<int>(ns)`, `Box<int> { value: none }`), and one
task or shape is made for each set of types used. The types a shape is given name no region, as its fields
don't: `Box<text>@r` holds its text in `r`. A program can span
several files: `include shapes/geometry` reads `shapes/geometry.vela`, named as `geometry.area(…)` (or brought in
with `include shapes/geometry (area)`, or only the names with braces, `{area}`), `=>` gives a module or a name
another name (`include fuel/prices => fuel`), and everything a file declares is public unless marked `priv`. Vela's
own library comes with the compiler and is included the same way, with a path starting `std`
(`include std/files {File}`). A
variant can be written through its kind, `Size.Small`. A shape or a kind has methods, written in its block below
its fields or variants and called on a value (`c.total()`); inside one, `.count` is a field of the value, `.this`
the value itself, and `@this` the region it lives in, and a method that changes its value says `alters` before its
name, so only a place that can change may call it. A shape may have `shared` fields and methods, one for the whole
program, named through it (`Tally.count`, `Point.new(…)`), fields that fall back on a value (`n: int = 0`), and
`read` fields only its methods change.

A parameter is read and never changed (`x: T`), or says how it is given: `hold x: T` is the task's own copy to
change, `hold& x: T` changes the caller's place, which the call marks, `f(&a)`, `bind x: T` is always a copy, and
`bind& x: T` reads the caller's place where it is. A call never gives one place twice where it changes it, and a
plain parameter larger than 64 bytes is read where the caller keeps it when nothing can change it meanwhile, with a
warning where that choice shows. `hold& y = &x` and `bind& y = &x` name a place in a block too. `==` compares an
array, a shape, or a tuple by where it is, and `~=` (and `<>`, its opposite) item by item; a `maybe` is compared
with `==` only with `none`, and with `~=` with a plain value too, which an empty one never matches. `&x` shows or
compares where a place is, `x.type` is its type as text, and whole numbers may be written in hex, octal, or
binary (`0xff`, `0o17`, `0b101`). A plan B giving `none` makes the whole a `maybe`, and a condition may join an
`is` with `and`, as in `when found is {v} and v > 0`. `each v in xs` goes through a slice as it was when the loop
started, and `each v in &xs` as it is at each pass. `block … fi` runs statements in a block of their own.

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

`vela <input.vela> [-o <output>] [--emit-c | --check | --quick]`: `-o` names the executable (`a.out` by default),
`--quick` builds it without optimizing the C (`-O0`, faster to build, slower to run),
`--emit-c` prints the generated C (or writes it to the `-o` file; neither may overwrite the program's files), `--check` only checks the program
(including that it has a `main`), and `-h`/`--help` and `--version` print the usage and the version. The exit status is 0 on success, 1
when the program can't be read or doesn't compile, and 2 for wrong arguments.

## Tests

```sh
cargo test                              # the compiler
CC=clang cargo test                     # with another C compiler
cargo test --workspace                  # the compiler and Vela Studio's crates
cargo clippy --workspace --all-targets
```

Programs in `tests/run/` must print their `.out` files (reading `NAME.in` as their input, when there is one), programs in `tests/fail/` must be rejected with
the error in their first line (a program with several mistakes lists one `; error:` line for each, and then
those must be exactly the mistakes reported), programs in `tests/trap/` must print their `.out` files and then
stop with the runtime error in their first line, and every file in `examples/` is checked against
`tests/examples/`.

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
| `runtime/vela_rt.h` | The small C runtime the generated code includes, with the bodies of the library's native tasks |
| `std/` | Vela's library, in Vela, which the compiler holds as it is built (`include std/files`) |
| `examples/` | Example programs |
| `tests/` | Program tests (`run/`, `fail/`, `trap/`, `examples/`), C runtime tests (`runtime/`), and Rust tests |
| `docs/` | The language reference, the regions guide, a comparison with other languages, design notes, the safety and may-waste lists, and the Vela Studio guide |
| `ide/` | Vela Studio: `analysis` (language support), `wasm` (it, for the browser), `server` (`vela-studio`), `web` (the page) |

## Documentation

- [The language](docs/vela.html)
- [Regions: memory in Vela](docs/regions.html), a guide and tutorial
- [Vela's library](docs/std.html), the modules that come with the compiler
- [Vela Studio](docs/ide.html)
- [Vela and other languages](docs/comparison.html)
- [Future directions](docs/future.html), and a [second design notebook](docs/future-2.html)
- [Safety](docs/safety.html): what the compiler checks, what stops a running program, and what isn't checked yet
- [Events](docs/events.html): an idea for running work at once, not built
- [May waste](docs/maywaste.html): features to keep an eye on, and to merge or drop if they never earn a use
