#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <pthread.h>

typedef struct {
    int64_t val;
    int has_item;
    pthread_mutex_t lock;
    pthread_cond_t not_empty;
    pthread_cond_t not_full;
} chan_t;

static void chan_init(chan_t *c) {
    c->val = 0;
    c->has_item = 0;
    pthread_mutex_init(&c->lock, NULL);
    pthread_cond_init(&c->not_empty, NULL);
    pthread_cond_init(&c->not_full, NULL);
}

static void chan_destroy(chan_t *c) {
    pthread_mutex_destroy(&c->lock);
    pthread_cond_destroy(&c->not_empty);
    pthread_cond_destroy(&c->not_full);
}

static void chan_send(chan_t *c, int64_t val) {
    pthread_mutex_lock(&c->lock);
    while (c->has_item) {
        pthread_cond_wait(&c->not_full, &c->lock);
    }
    c->val = val;
    c->has_item = 1;
    pthread_cond_signal(&c->not_empty);
    pthread_mutex_unlock(&c->lock);
}

static int64_t chan_recv(chan_t *c) {
    pthread_mutex_lock(&c->lock);
    while (!c->has_item) {
        pthread_cond_wait(&c->not_empty, &c->lock);
    }
    int64_t val = c->val;
    c->has_item = 0;
    pthread_cond_signal(&c->not_full);
    pthread_mutex_unlock(&c->lock);
    return val;
}

typedef struct {
    int id;
    chan_t *in_ch;
    chan_t *out_ch;
} worker_arg_t;

static void* worker(void *arg) {
    worker_arg_t *w = (worker_arg_t*)arg;
    for (;;) {
        int64_t token = chan_recv(w->in_ch);
        if (token == 0) {
            chan_send(w->out_ch, 0);
            return (void*)(intptr_t)w->id;
        }
        chan_send(w->out_ch, token - 1);
    }
}

int main(void) {
    int64_t passes = 100000;
    chan_t ch[4];
    for (int i = 0; i < 4; i++) {
        chan_init(&ch[i]);
    }

    pthread_t th[4];
    worker_arg_t args[4];
    for (int i = 0; i < 4; i++) {
        args[i].id = i;
        args[i].in_ch = &ch[i];
        args[i].out_ch = &ch[(i + 1) % 4];
        pthread_create(&th[i], NULL, worker, &args[i]);
    }

    chan_send(&ch[0], passes);

    void *res[4];
    for (int i = 0; i < 4; i++) {
        pthread_join(th[i], &res[i]);
    }
    for (int i = 0; i < 4; i++) {
        chan_destroy(&ch[i]);
    }

    printf("ring completed %ld passes: %ld %ld %ld %ld\n",
           (long)passes, (long)(intptr_t)res[0], (long)(intptr_t)res[1],
           (long)(intptr_t)res[2], (long)(intptr_t)res[3]);
    return 0;
}
