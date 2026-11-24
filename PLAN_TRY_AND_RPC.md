# Implementation Plan: Postfix `!`, Error Mapping Pipelines (`!>` / `!>>`), and JSON-RPC 2.0 Connection Mode

**Status:** Complete

## Overview & Design Decisions

1. **Postfix `!` (`ExprKind::Try`)**
   - Lex `!=` as `Token::Ne`, `!>` / `!>>` / ... as `Token::ErrPipe(count)`, and `!` as `Token::Bang`.
   - Parse `!` as a postfix suffix operator in `parse_suffixes_with`, allowing method chaining (`call()!.method()!.meth2()`), field access, indexing, `at r` / `@r`, and statements (`call()!`).
   - Prefix `!x` still produces `"unexpected '!'; Vela negates with 'not'"`.
   - Check that the inner expression returns `T ~ E_inner`, the enclosing task/method/lambda returns `R ~ E_task`, and `E_inner` fits or widens via kind imitation into `E_task`.
   - Generate C11 code that stores the intermediate `T ~ E_inner`, early-returns `{ .ok = false, .error = ... }` after running task exit hooks and cleaning up commitments/drop-hooks/regions if `!ok`, and yields the unwrapped `T`.

2. **Error Mapping Pipeline (`!>`, `!>>`, `!>>>`, ...)**
   - Parse `!>` / `!>>` at `PIPE_PRECEDENCE` with multiline continuation support.
   - Support optional failure parameter on `!>` (`count == 1`), positional failure parameter on `!>>` / `!>>>` (`count >= 2`), bare kinds/variants/tasks/shared constructors, inline `failure` keyword (`call() !> failure.whatever()`), and arrow binders (`call() !> e -> e.whatever()`).
   - Require the mapping step to return a plain failure type `E_out` (not `U ~ E_out`, `fail E_out`, `maybe E_out`, or `nothing`).
   - Support chaining (`call() !>> X.new("error:") !> Z.new()`), trailing Plan B (`{ failure f ... }`) which stops propagation (`T ~ Z` consumed by the Plan B), and dot chaining on the propagated result (`(call() !> X.new()).meth()!.meth2()`).

3. **JSON-RPC 2.0 Connection Mode (`std/web`)**
   - Include `std/json` in `std/web.vela`.
   - Add `RpcError`, `RpcFraming` (`Line` / `ContentLength`), `RpcRequest`, `RpcResponse`, `JsonRpcConn` (over TCP `Socket` or `WebSocket`), `RpcServer` (dispatching single requests, notifications, batches, HTTP POST, and WebSocket sessions), `Router.rpc`, `Router.ws_rpc`, and `Client.rpc_call` / `rpc_call` / `rpc_notify`.

---

## Phase Checklist

### Phase 1: Plan File Creation
- [x] Create `PLAN_TRY_AND_RPC.md` with full ordered phase breakdown.

### Phase 2: Lexer, AST, Parser & Module Resolution for `!` and `!>`
- [x] `src/lexer.rs`: Add `Token::Bang` and `Token::ErrPipe(usize)`, update `describe` and `scan`.
- [x] `src/ast.rs`: Add `FAILURE_VAL`, `ExprKind::Try`, `ExprKind::ErrPipe`.
- [x] `src/parser.rs`: Parse postfix `!`, `!>` / `!>>` pipelines, inline `failure` and `e -> expr` mappers, Plan B peeling, `parse_pipe` compatibility, and prefix `!` error message.
- [x] `src/modules.rs`: Resolve `ExprKind::Try` and `ExprKind::ErrPipe` (including bare `Shape`/`Kind` -> `shared new`, `Type.method` resolution, and skipping `FAILURE_VAL`).

### Phase 3: Type Checker, Codegen & IDE Analysis for `!` and `!>`
- [x] `src/check.rs`: Implement `check_try`, `check_err_pipe`, `check_doing` / `check_expr_stmt` updates, and AST walkers.
- [x] `src/codegen.rs`: Map `FAILURE_VAL` to `vela__failure`, implement `gen_try` and `gen_err_pipe` with region/commitment/hook cleanup and kind widening.
- [x] `ide/analysis/`: Update `scan.rs`, `types.rs`, and `infer.rs` for `Token::Bang`, `Token::ErrPipe`, `ExprKind::Try`, and `ExprKind::ErrPipe`.

### Phase 4: JSON-RPC 2.0 Connection Mode in `std/web.vela`
- [x] `std/web.vela`: Implement `RpcError`, `RpcFraming`, `RpcRequest`, `RpcResponse`, `JsonRpcConn`, `RpcServer`, router routes, and client helpers.

### Phase 5: Tests, Examples & Documentation
- [x] Update `tests/lexer.rs`.
- [x] Add `tests/run/try_and_err_pipe.vela` + `.out` and negative tests in `tests/fail/`.
- [x] Add `tests/run/web_json_rpc.vela` + `.out`.
- [x] Add runnable examples (`examples/error_handling/err_propagation.vela`, `examples/io_and_system/json_rpc_service.vela`) and `tests/examples/*.out` snapshots.
- [x] Update `docs/vela.html`, `docs/std.html`, `docs/web.html`, and `docs/future.html`.
- [x] Run full `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace --all-targets` and update IDE snapshots if needed.
