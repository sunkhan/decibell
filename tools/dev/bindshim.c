/*
 * bindshim: an LD_PRELOAD library that moves a server's listening sockets
 * without touching its code. The dev stack (tools/dev/local-stack.sh) needs
 * three community servers and a central on one machine, but both servers
 * hard-code their ports (central 8080; a community 8082 TCP, 8083/8084 UDP,
 * 8085 HTTPS) and bind the IPv4 wildcard address.
 *
 *   DECIBELL_DEV_BIND_IP=127.0.0.11      a bind() to 0.0.0.0 goes to this
 *                                        address instead (any loopback
 *                                        127.x.y.z works on Linux untouched)
 *   DECIBELL_DEV_PORT_MAP=8080:18080,... a bind() to the left port goes to
 *                                        the right one
 *
 * Only IPv4 bind() calls are rewritten; connect() and everything else pass
 * straight through. Unset variables: the library does nothing.
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dlfcn.h>
#include <netinet/in.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

typedef int (*bind_fn)(int, const struct sockaddr *, socklen_t);

static int map_port(int port) {
    const char *map = getenv("DECIBELL_DEV_PORT_MAP");
    if (!map) return port;
    const char *p = map;
    while (*p) {
        char *end;
        long from = strtol(p, &end, 10);
        if (end == p || *end != ':') return port;
        p = end + 1;
        long to = strtol(p, &end, 10);
        if (end == p) return port;
        if (from == port && to > 0 && to < 65536) return (int)to;
        p = end;
        if (*p == ',') p++;
    }
    return port;
}

int bind(int fd, const struct sockaddr *addr, socklen_t len) {
    static bind_fn real_bind;
    if (!real_bind) real_bind = (bind_fn)dlsym(RTLD_NEXT, "bind");
    if (addr && addr->sa_family == AF_INET && len >= (socklen_t)sizeof(struct sockaddr_in)) {
        struct sockaddr_in in;
        memcpy(&in, addr, sizeof in);
        const char *ip = getenv("DECIBELL_DEV_BIND_IP");
        if (ip && *ip && in.sin_addr.s_addr == htonl(INADDR_ANY)) {
            struct in_addr a;
            if (inet_pton(AF_INET, ip, &a) == 1) in.sin_addr = a;
        }
        int port = ntohs(in.sin_port);
        if (port != 0) in.sin_port = htons((unsigned short)map_port(port));
        return real_bind(fd, (const struct sockaddr *)&in, sizeof in);
    }
    return real_bind(fd, addr, len);
}
