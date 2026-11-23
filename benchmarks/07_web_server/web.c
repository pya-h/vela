#include <arpa/inet.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static int server_fd = -1;

static void *worker(void *arg) {
    (void)arg;
    const char *resp_json =
        "HTTP/1.1 200 OK\r\n"
        "Content-Type: application/json\r\n"
        "Content-Length: 26\r\n"
        "Connection: close\r\n\r\n"
        "{\"status\":\"ok\",\"value\":42}";
    const char *resp_health =
        "HTTP/1.1 200 OK\r\n"
        "Content-Type: text/plain\r\n"
        "Content-Length: 2\r\n"
        "Connection: close\r\n\r\n"
        "ok";
    const char *resp_404 =
        "HTTP/1.1 404 Not Found\r\n"
        "Content-Length: 9\r\n"
        "Connection: close\r\n\r\n"
        "not found";
    size_t len_json = strlen(resp_json);
    size_t len_health = strlen(resp_health);
    size_t len_404 = strlen(resp_404);

    char buf[2048];
    for (;;) {
        int client = accept(server_fd, NULL, NULL);
        if (client < 0) continue;

        ssize_t total = 0;
        while (total < (ssize_t)sizeof(buf) - 1) {
            ssize_t n = recv(client, buf + total, sizeof(buf) - 1 - (size_t)total, 0);
            if (n <= 0) break;
            total += n;
            buf[total] = '\0';
            if (strstr(buf, "\r\n\r\n")) break;
        }

        if (strncmp(buf, "GET /json ", 10) == 0) {
            send(client, resp_json, len_json, 0);
        } else if (strncmp(buf, "GET /health ", 12) == 0) {
            send(client, resp_health, len_health, 0);
        } else {
            send(client, resp_404, len_404, 0);
        }
        close(client);
    }
    return NULL;
}

int main(void) {
    signal(SIGPIPE, SIG_IGN);
    server_fd = socket(AF_INET, SOCK_STREAM, 0);
    int yes = 1;
    setsockopt(server_fd, SOL_SOCKET, SO_REUSEADDR, &yes, sizeof(yes));

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(18260);
    inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);

    if (bind(server_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("bind");
        return 1;
    }
    if (listen(server_fd, 128) < 0) {
        perror("listen");
        return 1;
    }

    pthread_t th[4];
    for (int i = 0; i < 4; i++) {
        pthread_create(&th[i], NULL, worker, NULL);
    }
    for (int i = 0; i < 4; i++) {
        pthread_join(th[i], NULL);
    }
    return 0;
}
