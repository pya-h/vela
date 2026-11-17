#include "vela_rt.h"

static int failures = 0;

static void check(bool condition, const char* message) {
    if (!condition) {
        printf("commitment runtime test failed: %s\n", message);
        failures++;
    }
}

static void sleep_ms(int64_t ms) {
    struct timespec req;
    req.tv_sec = (time_t)(ms / 1000);
    req.tv_nsec = (long)((ms % 1000) * 1000000L);
    nanosleep(&req, NULL);
}

typedef struct basic_ctx {
    int64_t handle;
    int64_t input;
    int64_t delay_ms;
} basic_ctx;

static void basic_worker(void* raw) {
    basic_ctx ctx = *(basic_ctx*)raw;
    free(raw);
    int64_t prev = vela__rt__current_commitment;
    vela__rt__current_commitment = ctx.handle;
    if (ctx.delay_ms > 0) {
        sleep_ms(ctx.delay_ms);
    }
    int64_t out = ctx.input * 2 + 1;
    vela__rt__commit_complete(ctx.handle, &out);
    vela__rt__current_commitment = prev;
}

typedef struct cancel_ctx {
    int64_t handle;
    int64_t gate_ch;
} cancel_ctx;

static void cancel_worker(void* raw) {
    cancel_ctx ctx = *(cancel_ctx*)raw;
    free(raw);
    int64_t prev = vela__rt__current_commitment;
    vela__rt__current_commitment = ctx.handle;
    int ticks = 0;
    while (!vela__rt__commit_is_cancelled() && ticks < 200) {
        sleep_ms(2);
        ticks++;
    }
    int64_t saw_cancel = vela__rt__commit_is_cancelled() ? 1 : 0;
    vela__rt__chan_send(ctx.gate_ch, &saw_cancel);
    vela__rt__commit_complete(ctx.handle, &saw_cancel);
    vela__rt__current_commitment = prev;
}

typedef struct nested_ctx {
    int64_t handle;
    int64_t depth;
} nested_ctx;

static void nested_worker(void* raw) {
    nested_ctx ctx = *(nested_ctx*)raw;
    free(raw);
    int64_t prev = vela__rt__current_commitment;
    vela__rt__current_commitment = ctx.handle;
    int64_t result = 1;
    if (ctx.depth > 0) {
        int64_t child = vela__rt__commit_create((int64_t)sizeof(int64_t));
        nested_ctx* next = (nested_ctx*)malloc(sizeof(nested_ctx));
        next->handle = child;
        next->depth = ctx.depth - 1;
        vela__rt__commit_start(nested_worker, next);

        int64_t child_val = 0;
        vela__rt__commit_wait(child, &child_val);
        vela__rt__commit_scope_cleanup(child);
        check(vela__rt__current_commitment == ctx.handle, "current commitment preserved across nested wait");
        result = child_val + 1;
    }
    vela__rt__commit_complete(ctx.handle, &result);
    vela__rt__current_commitment = prev;
}

int main(void) {
    /* 1. Basic wait + scope cleanup */
    int64_t c1 = vela__rt__commit_create((int64_t)sizeof(int64_t));
    basic_ctx* ctx1 = (basic_ctx*)malloc(sizeof(basic_ctx));
    ctx1->handle = c1;
    ctx1->input = 20;
    ctx1->delay_ms = 5;
    vela__rt__commit_start(basic_worker, ctx1);
    int64_t out1 = 0;
    vela__rt__commit_wait(c1, &out1);
    check(out1 == 41, "basic commit_wait receives value");
    vela__rt__commit_scope_cleanup(c1);

    /* 2. Non-blocking poll */
    int64_t c2 = vela__rt__commit_create((int64_t)sizeof(int64_t));
    int64_t polled = 0;
    check(!vela__rt__commit_poll(c2, &polled), "poll returns false before completion");
    basic_ctx* ctx2 = (basic_ctx*)malloc(sizeof(basic_ctx));
    ctx2->handle = c2;
    ctx2->input = 10;
    ctx2->delay_ms = 0;
    vela__rt__commit_start(basic_worker, ctx2);
    for (int i = 0; i < 200; i++) {
        if (vela__rt__commit_poll(c2, &polled)) {
            break;
        }
        sleep_ms(2);
    }
    check(polled == 21, "poll observes completed value");
    vela__rt__commit_scope_cleanup(c2);

    /* 3. Cancellation & structured scope cleanup */
    int64_t gate = vela__rt__chan_new(1, sizeof(int64_t));
    int64_t c3 = vela__rt__commit_create((int64_t)sizeof(int64_t));
    cancel_ctx* ctx3 = (cancel_ctx*)malloc(sizeof(cancel_ctx));
    ctx3->handle = c3;
    ctx3->gate_ch = gate;
    vela__rt__commit_start(cancel_worker, ctx3);
    vela__rt__commit_scope_cleanup(c3);
    int64_t saw = 0;
    check(vela__rt__chan_recv(gate, &saw) && saw == 1, "scope cleanup cancels and joins unawaited worker");
    vela__rt__chan_close(gate);

    /* 4. Racing commitments in select */
    int64_t fast = vela__rt__commit_create((int64_t)sizeof(int64_t));
    int64_t slow = vela__rt__commit_create((int64_t)sizeof(int64_t));
    basic_ctx* fctx = (basic_ctx*)malloc(sizeof(basic_ctx));
    fctx->handle = fast;
    fctx->input = 7;
    fctx->delay_ms = 2;
    basic_ctx* sctx = (basic_ctx*)malloc(sizeof(basic_ctx));
    sctx->handle = slow;
    sctx->input = 99;
    sctx->delay_ms = 35;
    vela__rt__commit_start(basic_worker, fctx);
    vela__rt__commit_start(basic_worker, sctx);

    int64_t r_fast = 0, r_slow = 0;
    vela__rt__select_arm arms[2] = {
        { fast, VELA__RT__SELECT_RECV, &r_fast },
        { slow, VELA__RT__SELECT_RECV, &r_slow }
    };
    int64_t winner = vela__rt__chan_select(arms, 2, false, true, 1000);
    check(winner == 0 && r_fast == 15, "select picks fast commitment");
    if (winner != 1) {
        vela__rt__commit_cancel(slow);
    }
    vela__rt__commit_scope_cleanup(slow);
    vela__rt__commit_scope_cleanup(fast);

    /* 5. Deep nested waits exceeding fixed pool thread count */
    int64_t deep = vela__rt__commit_create((int64_t)sizeof(int64_t));
    nested_ctx* nctx = (nested_ctx*)malloc(sizeof(nested_ctx));
    nctx->handle = deep;
    nctx->depth = 16;
    vela__rt__commit_start(nested_worker, nctx);
    int64_t depth_sum = 0;
    vela__rt__commit_wait(deep, &depth_sum);
    check(depth_sum == 17, "16-deep nested commitment wait completes without pool deadlock");
    vela__rt__commit_scope_cleanup(deep);

    vela__rt__sched_stop();

    if (failures == 0) {
        printf("ok\n");
    }
    return failures == 0 ? 0 : 1;
}
