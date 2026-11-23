#include <arpa/inet.h>
#include <netinet/tcp.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

typedef struct {
    int port;
    int reqs;
    int ok;
    int fail;
    double *lat_us;
} worker_arg_t;

static double now_sec(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec * 1e-9;
}

static int cmp_double(const void *a, const void *b) {
    double da = *(const double *)a;
    double db = *(const double *)b;
    return (da > db) - (da < db);
}

static void *worker(void *p) {
    worker_arg_t *a = (worker_arg_t *)p;
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)a->port);
    inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);

    const char *req = "GET /json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    size_t req_len = strlen(req);
    char buf[2048];

    for (int i = 0; i < a->reqs; i++) {
        double r0 = now_sec();
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0) {
            a->fail++;
            continue;
        }
        int yes = 1;
        setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &yes, sizeof(yes));
        if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
            close(fd);
            a->fail++;
            continue;
        }
        size_t sent = 0;
        int send_ok = 1;
        while (sent < req_len) {
            ssize_t w = send(fd, req + sent, req_len - sent, 0);
            if (w <= 0) {
                send_ok = 0;
                break;
            }
            sent += (size_t)w;
        }
        ssize_t total = 0;
        if (send_ok) {
            while (total < (ssize_t)sizeof(buf) - 1) {
                ssize_t n = recv(fd, buf + total, sizeof(buf) - 1 - (size_t)total, 0);
                if (n <= 0) break;
                total += n;
            }
        }
        buf[total] = '\0';
        close(fd);
        double r1 = now_sec();

        if (send_ok && strstr(buf, "200 OK") && strstr(buf, "\"status\":\"ok\"")) {
            a->lat_us[a->ok] = (r1 - r0) * 1e6;
            a->ok++;
        } else {
            a->fail++;
        }
    }
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: %s <port> <threads> <total_requests>\n", argv[0]);
        return 2;
    }
    int port = atoi(argv[1]);
    int threads = atoi(argv[2]);
    int total_reqs = atoi(argv[3]);
    if (threads < 1) threads = 1;
    if (threads > 64) threads = 64;

    pthread_t th[64];
    worker_arg_t args[64];
    int base_reqs = total_reqs / threads;
    int rem = total_reqs % threads;

    for (int i = 0; i < threads; i++) {
        int r = base_reqs + (i < rem ? 1 : 0);
        args[i].port = port;
        args[i].reqs = r;
        args[i].ok = 0;
        args[i].fail = 0;
        args[i].lat_us = (double *)calloc((size_t)r, sizeof(double));
    }

    double t0 = now_sec();
    for (int i = 0; i < threads; i++) {
        pthread_create(&th[i], NULL, worker, &args[i]);
    }
    for (int i = 0; i < threads; i++) {
        pthread_join(th[i], NULL);
    }
    double dt = now_sec() - t0;

    int total_ok = 0;
    int total_fail = 0;
    for (int i = 0; i < threads; i++) {
        total_ok += args[i].ok;
        total_fail += args[i].fail;
    }

    double *all_lat = (double *)malloc((size_t)(total_ok > 0 ? total_ok : 1) * sizeof(double));
    int pos = 0;
    double sum_lat = 0.0;
    for (int i = 0; i < threads; i++) {
        for (int j = 0; j < args[i].ok; j++) {
            all_lat[pos++] = args[i].lat_us[j];
            sum_lat += args[i].lat_us[j];
        }
        free(args[i].lat_us);
    }

    double mean_us = total_ok > 0 ? sum_lat / total_ok : 0.0;
    double p95_us = 0.0;
    if (total_ok > 0) {
        qsort(all_lat, (size_t)total_ok, sizeof(double), cmp_double);
        size_t idx95 = (size_t)((double)(total_ok - 1) * 0.95);
        p95_us = all_lat[idx95];
    }
    free(all_lat);

    double rps = dt > 0.0 ? (double)total_ok / dt : 0.0;
    printf("{\"ok\": %d, \"fail\": %d, \"elapsed\": %.6f, \"rps\": %.2f, \"mean_us\": %.2f, \"p95_us\": %.2f}\n",
           total_ok, total_fail, dt, rps, mean_us, p95_us);
    return total_fail > 0 ? 1 : 0;
}
