/* netlog.so: an LD_PRELOAD shim that logs every socket the process opens and every connection,
 * datagram and name lookup it makes (to $NETLOG_FILE), and passes each call on unchanged. For
 * ship-isolated.sh: "the ship binary contacts no network host when started without credentials".
 * Calls made through libc's symbols are seen (Rust's std, tokio / mio, GPUI, zbus all use them);
 * raw syscalls would not be. */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

static void logf_(const char *fmt, ...) {
    const char *path = getenv("NETLOG_FILE");
    if (!path) return;
    char line[512];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(line, sizeof line - 1, fmt, ap);
    va_end(ap);
    if (n < 0) return;
    if (n > (int)sizeof line - 2) n = sizeof line - 2;
    line[n++] = '\n';
    int fd = open(path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
    if (fd >= 0) { (void)!write(fd, line, n); close(fd); }
}

static const char *family(int f) {
    switch (f) { case AF_UNIX: return "unix"; case AF_INET: return "inet"; case AF_INET6: return "inet6";
                 case AF_NETLINK: return "netlink"; default: return "other"; }
}

static void describe(const struct sockaddr *a, char *out, size_t len) {
    if (!a) { snprintf(out, len, "-"); return; }
    if (a->sa_family == AF_INET) {
        const struct sockaddr_in *s = (const void *)a; char ip[64];
        inet_ntop(AF_INET, &s->sin_addr, ip, sizeof ip); snprintf(out, len, "inet %s:%d", ip, ntohs(s->sin_port));
    } else if (a->sa_family == AF_INET6) {
        const struct sockaddr_in6 *s = (const void *)a; char ip[64];
        inet_ntop(AF_INET6, &s->sin6_addr, ip, sizeof ip); snprintf(out, len, "inet6 [%s]:%d", ip, ntohs(s->sin6_port));
    } else if (a->sa_family == AF_UNIX) {
        const struct sockaddr_un *s = (const void *)a;
        snprintf(out, len, "unix %s%s", s->sun_path[0] ? "" : "@", s->sun_path[0] ? s->sun_path : s->sun_path + 1);
    } else snprintf(out, len, "%s", family(a->sa_family));
}

int socket(int domain, int type, int protocol) {
    static int (*real)(int, int, int); if (!real) real = dlsym(RTLD_NEXT, "socket");
    int fd = real(domain, type, protocol);
    logf_("socket %s type=%d -> %d", family(domain), type & 0xff, fd);
    return fd;
}

int connect(int fd, const struct sockaddr *a, socklen_t len) {
    static int (*real)(int, const struct sockaddr *, socklen_t); if (!real) real = dlsym(RTLD_NEXT, "connect");
    char d[160]; describe(a, d, sizeof d); logf_("connect %d %s", fd, d);
    return real(fd, a, len);
}

ssize_t sendto(int fd, const void *b, size_t n, int f, const struct sockaddr *a, socklen_t len) {
    static ssize_t (*real)(int, const void *, size_t, int, const struct sockaddr *, socklen_t); if (!real) real = dlsym(RTLD_NEXT, "sendto");
    if (a) { char d[160]; describe(a, d, sizeof d); logf_("sendto %d %s", fd, d); }
    return real(fd, b, n, f, a, len);
}

ssize_t sendmsg(int fd, const struct msghdr *m, int f) {
    static ssize_t (*real)(int, const struct msghdr *, int); if (!real) real = dlsym(RTLD_NEXT, "sendmsg");
    if (m && m->msg_name) { char d[160]; describe(m->msg_name, d, sizeof d); logf_("sendmsg %d %s", fd, d); }
    return real(fd, m, f);
}

int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints, struct addrinfo **res) {
    static int (*real)(const char *, const char *, const struct addrinfo *, struct addrinfo **); if (!real) real = dlsym(RTLD_NEXT, "getaddrinfo");
    logf_("getaddrinfo %s %s", node ? node : "-", service ? service : "-");
    return real(node, service, hints, res);
}
