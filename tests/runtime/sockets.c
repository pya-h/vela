#include "vela_rt.h"

static int failures = 0;

static void check(bool condition, const char* message) {
    if (!condition) {
        printf("sockets test failed: %s\n", message);
        failures++;
    }
}

int main(void) {
    vela__rt__region r[1] = { VELA__RT__REGION_EMPTY };

    int64_t server = vela__rt__web_socket_open(r);
    check(server >= 0, "server socket open");

    vela__rt__text host = VELA__RT__TEXT("127.0.0.1");
    int64_t bind_res = vela__rt__web_socket_bind(server, host, 18090);
    check(bind_res == 0, "server socket bind");

    int64_t listen_res = vela__rt__web_socket_listen(server, 16);
    check(listen_res == 0, "server socket listen");

    int64_t client = vela__rt__web_socket_open(r);
    check(client >= 0, "client socket open");

    int64_t connect_res = vela__rt__web_socket_connect(client, host, 18090);
    check(connect_res == 0 || connect_res == VELA__RT__SOCK_WOULD_BLOCK, "client connect");

    /* Poll server until incoming connection is ready. */
    int64_t poll_srv = vela__rt__web_socket_poll(server, 1, 1000);
    check((poll_srv & 1) != 0, "server poll read ready");

    int64_t accepted = vela__rt__web_socket_accept(server, r);
    check(accepted >= 0, "server accept");

    /* Client writes data. */
    vela__rt__text ping = VELA__RT__TEXT("ping\n");
    int64_t wrote = vela__rt__web_socket_write(client, ping);
    check(wrote == ping.len, "client write ping");

    /* Server polls and reads data. */
    int64_t poll_acc = vela__rt__web_socket_poll(accepted, 1, 1000);
    check((poll_acc & 1) != 0, "accepted poll read ready");

    vela__rt__text recvd = vela__rt__web_socket_read(accepted, 64, r);
    check(recvd.len == ping.len, "accepted read length");
    check(memcmp(recvd.bytes, ping.bytes, (size_t)ping.len) == 0, "accepted read content");

    /* Server replies with pong. */
    vela__rt__text pong = VELA__RT__TEXT("pong\n");
    wrote = vela__rt__web_socket_write(accepted, pong);
    check(wrote == pong.len, "server write pong");

    /* Client polls and reads pong. */
    int64_t poll_cli = vela__rt__web_socket_poll(client, 1, 1000);
    check((poll_cli & 1) != 0, "client poll read ready");

    recvd = vela__rt__web_socket_read(client, 64, r);
    check(recvd.len == pong.len, "client read length");
    check(memcmp(recvd.bytes, pong.bytes, (size_t)pong.len) == 0, "client read content");

    /* Close client and verify accepted socket reads EOF. */
    vela__rt__web_socket_close(client);
    recvd = vela__rt__web_socket_read(accepted, 64, r);
    check(recvd.len == 0, "accepted read EOF on close");
    check(vela__rt__web_socket_status() == VELA__RT__SOCK_CLOSED, "accepted status is CLOSED");

    /* Verify time and poll table primitives. */
    int64_t t1 = vela__rt__web_time_ms();
    check(t1 > 0, "web_time_ms positive");
    vela__rt__web_sleep_ms(1);
    int64_t t2 = vela__rt__web_time_ms();
    check(t2 >= t1, "web_time_ms monotonic");

    vela__rt__web_poll_clear();
    vela__rt__web_poll_add(server, 1);
    check(!vela__rt__web_poll_is_readable(server), "poll is not readable before wait");
    check(!vela__rt__web_poll_is_writable(server), "poll is not writable before wait");
    int64_t poll_res = vela__rt__web_poll_wait(1);
    check(poll_res == 0, "web_poll_wait timeout on idle server");
    check(vela__rt__web_poll_revents(server) == 0, "revents empty on timeout");
    vela__rt__web_poll_remove(server);

    vela__rt__web_socket_close(accepted);
    vela__rt__web_socket_close(server);

    vela__rt__region_free(r);
    if (failures == 0) {
        puts("ok");
    }
    return failures != 0;
}
