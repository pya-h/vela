#include "vela_rt.h"

static int failures = 0;

static void check(bool condition, const char* message) {
    if (!condition) {
        printf("select test failed: %s\n", message);
        failures++;
    }
}

typedef struct wake_arg {
    int64_t ch;
    int64_t delay_ms;
    int64_t val;
    bool close_after;
    bool consume_instead;
} wake_arg;

static void sleep_ms(int64_t ms) {
    struct timespec req;
    req.tv_sec = (time_t)(ms / 1000);
    req.tv_nsec = (long)((ms % 1000) * 1000000L);
    nanosleep(&req, NULL);
}

static void* worker_fn(void* raw) {
    wake_arg* arg = (wake_arg*)raw;
    sleep_ms(arg->delay_ms);
    if (arg->consume_instead) {
        int64_t tmp = 0;
        vela__rt__chan_recv(arg->ch, &tmp);
    } else if (arg->close_after) {
        vela__rt__chan_close(arg->ch);
    } else {
        vela__rt__chan_send(arg->ch, &arg->val);
    }
    return NULL;
}

int main(void) {
    /* 1. Non-blocking otherwise when empty */
    int64_t c1 = vela__rt__chan_new(2, sizeof(int64_t));
    int64_t c2 = vela__rt__chan_new(2, sizeof(int64_t));
    int64_t out1 = 0, out2 = 0;
    vela__rt__select_arm arms[2] = {
        { c1, VELA__RT__SELECT_RECV, &out1 },
        { c2, VELA__RT__SELECT_RECV, &out2 }
    };
    check(vela__rt__chan_select(arms, 2, true, false, 0) == VELA__RT__SELECT_OTHERWISE,
          "empty select picks otherwise");

    /* 2. Ready receive + fairness across two ready channels */
    int64_t v = 10;
    vela__rt__chan_send(c1, &v);
    v = 20;
    vela__rt__chan_send(c2, &v);
    int64_t w1 = vela__rt__chan_select(arms, 2, false, false, 0);
    int64_t w2 = vela__rt__chan_select(arms, 2, false, false, 0);
    check((w1 == 0 && w2 == 1 && out1 == 10 && out2 == 20)
       || (w1 == 1 && w2 == 0 && out2 == 20 && out1 == 10),
          "both ready receive arms drain fairly");

    /* 3. Zero-ms and timed timeouts */
    check(vela__rt__chan_select(arms, 2, false, true, 0) == VELA__RT__SELECT_TIMEOUT,
          "0ms timeout returns immediately");
    check(vela__rt__chan_select(arms, 2, false, true, 15) == VELA__RT__SELECT_TIMEOUT,
          "15ms timeout expires");

    /* 4. Empty arm list with timeout and otherwise */
    check(vela__rt__chan_select(NULL, 0, true, false, 0) == VELA__RT__SELECT_OTHERWISE,
          "0-arm otherwise");
    check(vela__rt__chan_select(NULL, 0, false, true, 5) == VELA__RT__SELECT_TIMEOUT,
          "0-arm timeout");

    /* 5. Asynchronous wake on receive */
    pthread_t th;
    wake_arg arg_recv = { c2, 15, 777, false, false };
    out2 = 0;
    check(pthread_create(&th, NULL, worker_fn, &arg_recv) == 0, "spawn recv wake thread");
    int64_t got = vela__rt__chan_select(arms, 2, false, true, 1000);
    pthread_join(th, NULL);
    check(got == 1 && out2 == 777, "woken by async send to c2");

    /* 6. Asynchronous wake on send to full bounded channel */
    int64_t full = vela__rt__chan_new(1, sizeof(int64_t));
    int64_t fill_v = 11;
    vela__rt__chan_send(full, &fill_v);
    int64_t send_v = 99;
    vela__rt__select_arm send_arms[2] = {
        { c1, VELA__RT__SELECT_RECV, &out1 },
        { full, VELA__RT__SELECT_SEND, &send_v }
    };
    wake_arg arg_cons = { full, 15, 0, false, true };
    check(pthread_create(&th, NULL, worker_fn, &arg_cons) == 0, "spawn consumer thread");
    got = vela__rt__chan_select(send_arms, 2, false, true, 1000);
    pthread_join(th, NULL);
    check(got == 1, "send arm commits once space frees");
    int64_t check_v = 0;
    check(vela__rt__chan_try_recv(full, &check_v) && check_v == 99, "committed value matches");

    /* 7. Duplicate handle across arms */
    int64_t dup_out = 0;
    int64_t dup_in = 333;
    vela__rt__select_arm dup_arms[2] = {
        { full, VELA__RT__SELECT_RECV, &dup_out },
        { full, VELA__RT__SELECT_SEND, &dup_in }
    };
    got = vela__rt__chan_select(dup_arms, 2, false, false, 0);
    check(got == 1, "duplicate handle send succeeds safely");
    int64_t drained_dup = 0;
    check(vela__rt__chan_try_recv(full, &drained_dup) && drained_dup == 333, "dup send payload intact");
    vela__rt__chan_close(full);

    /* 8. Closed channel skips closed arm, matches none arm, and handles all-closed */
    vela__rt__chan_close(c1);
    vela__rt__select_arm closed_and_open[2] = {
        { c1, VELA__RT__SELECT_RECV, &out1 },
        { c2, VELA__RT__SELECT_RECV, &out2 }
    };
    wake_arg arg_c2 = { c2, 15, 444, false, false };
    out2 = 0;
    check(pthread_create(&th, NULL, worker_fn, &arg_c2) == 0, "spawn open-arm wake thread");
    got = vela__rt__chan_select(closed_and_open, 2, false, true, 1000);
    pthread_join(th, NULL);
    check(got == 1 && out2 == 444, "closed arm skipped while open arm receives");

    vela__rt__select_arm with_none[2] = {
        { c2, VELA__RT__SELECT_RECV, &out2 },
        { c1, VELA__RT__SELECT_RECV_NONE, NULL }
    };
    check(vela__rt__chan_select(with_none, 2, false, true, 100) == 1,
          "closed+destroyed handle matches RECV_NONE");

    /* Drain last buffered item of a closed channel without deadlocking chan_destroy */
    int64_t last = 555;
    vela__rt__chan_send(c2, &last);
    vela__rt__chan_close(c2);
    out2 = 0;
    check(vela__rt__chan_select(closed_and_open, 2, false, false, 0) == 1 && out2 == 555,
          "draining closed channel in select frees cleanly");

    /* All arms closed -> VELA__RT__SELECT_ALL_CLOSED */
    check(vela__rt__chan_select(closed_and_open, 2, false, false, 0) == VELA__RT__SELECT_ALL_CLOSED,
          "all closed returns ALL_CLOSED");

    /* Async close wakes RECV_NONE waiter */
    int64_t c3 = vela__rt__chan_new(1, sizeof(int64_t));
    vela__rt__select_arm wait_close[1] = {
        { c3, VELA__RT__SELECT_RECV_NONE, NULL }
    };
    wake_arg arg_cls = { c3, 15, 0, true, false };
    check(pthread_create(&th, NULL, worker_fn, &arg_cls) == 0, "spawn close thread");
    check(vela__rt__chan_select(wait_close, 1, false, true, 1000) == 0,
          "async close wakes RECV_NONE arm");
    pthread_join(th, NULL);

    /* 9. Heap fallback (> 16 arms) */
    int64_t chans[20];
    int64_t outs[20];
    vela__rt__select_arm many[20];
    for (int i = 0; i < 20; i++) {
        chans[i] = vela__rt__chan_new(1, sizeof(int64_t));
        outs[i] = 0;
        many[i].handle = chans[i];
        many[i].op = VELA__RT__SELECT_RECV;
        many[i].item_ptr = &outs[i];
    }
    int64_t msg19 = 1919;
    vela__rt__chan_send(chans[19], &msg19);
    got = vela__rt__chan_select(many, 20, false, false, 0);
    check(got == 19 && outs[19] == 1919, "20-arm heap fallback receives");
    for (int i = 0; i < 20; i++) {
        vela__rt__chan_close(chans[i]);
    }

    if (failures == 0) {
        printf("ok\n");
    }
    return failures == 0 ? 0 : 1;
}

