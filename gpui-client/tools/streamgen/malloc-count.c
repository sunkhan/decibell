// malloc-count: counts heap allocations per thread in an unmodified binary, to check the "no allocation per
// frame" rule on the paths a headless test cannot reach (the GPU decode path, the renderer).
//
//   cc -O2 -shared -fPIC tools/streamgen/malloc-count.c -o vendor/bin/malloc-count.so -ldl
//   LD_PRELOAD=vendor/bin/malloc-count.so target/release/decibell --screen video-test --dbv ... &
//   kill -USR2 $!      # prints "malloc-count <thread name> <tid> <allocations so far>" per thread on stderr
//
// Two signals a known number of frames apart give allocations per frame per thread. Counts malloc, calloc,
// realloc, posix_memalign, aligned_alloc and memalign of every caller in the process: Rust code, FFmpeg, wgpu and
// the GPU driver alike. No locks; a thread's counter is a slot it claims on its first allocation.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#define SLOTS 512
static struct { _Atomic int tid; _Atomic unsigned long count; } slots[SLOTS];
static __thread int my_slot = -1;

static void *(*real_malloc)(size_t);
static void *(*real_calloc)(size_t, size_t);
static void *(*real_realloc)(void *, size_t);
static int (*real_posix_memalign)(void **, size_t, size_t);
static void *(*real_aligned_alloc)(size_t, size_t);
static void *(*real_memalign)(size_t, size_t);
static void (*real_free)(void *);

// dlsym allocates while we resolve the real functions: serve that from a static arena.
static char arena[1 << 16];
static size_t arena_used;
static int resolving;

static void count(void)
{
    if (my_slot < 0) {
        int tid = (int)syscall(SYS_gettid);
        for (int i = 0; i < SLOTS; ++i) {
            int expected = 0;
            if (atomic_compare_exchange_strong(&slots[i].tid, &expected, tid)) { my_slot = i; break; }
        }
        if (my_slot < 0) return;
    }
    atomic_fetch_add_explicit(&slots[my_slot].count, 1, memory_order_relaxed);
}

static void dump(int sig)
{
    (void)sig;
    char line[160];
    for (int i = 0; i < SLOTS; ++i) {
        int tid = atomic_load(&slots[i].tid);
        if (!tid) continue;
        char path[64], name[32] = "?";
        snprintf(path, sizeof path, "/proc/self/task/%d/comm", tid);
        FILE *f = fopen(path, "r");
        if (f) { if (fgets(name, sizeof name, f)) name[strcspn(name, "\n")] = 0; fclose(f); }
        int n = snprintf(line, sizeof line, "malloc-count %s %d %lu\n", name, tid, atomic_load(&slots[i].count));
        if (write(2, line, (size_t)n) < 0) return;
    }
    if (write(2, "malloc-count end\n", 17) < 0) return;
}

static void resolve(void)
{
    if (real_malloc || resolving) return;
    resolving = 1;
    real_malloc = dlsym(RTLD_NEXT, "malloc");
    real_calloc = dlsym(RTLD_NEXT, "calloc");
    real_realloc = dlsym(RTLD_NEXT, "realloc");
    real_posix_memalign = dlsym(RTLD_NEXT, "posix_memalign");
    real_aligned_alloc = dlsym(RTLD_NEXT, "aligned_alloc");
    real_memalign = dlsym(RTLD_NEXT, "memalign");
    real_free = dlsym(RTLD_NEXT, "free");
    resolving = 0;
    signal(SIGUSR2, dump);
}

static void *from_arena(size_t size)
{
    size = (size + 15) & ~(size_t)15;
    if (arena_used + size > sizeof arena) return NULL;
    void *p = arena + arena_used;
    arena_used += size;
    return p;
}

void *malloc(size_t size) { resolve(); if (!real_malloc) return from_arena(size); count(); return real_malloc(size); }
void *calloc(size_t n, size_t size)
{
    resolve();
    if (!real_calloc) { void *p = from_arena(n * size); if (p) memset(p, 0, n * size); return p; }
    count();
    return real_calloc(n, size);
}
void *realloc(void *p, size_t size)
{
    resolve();
    if ((char *)p >= arena && (char *)p < arena + sizeof arena) {
        void *q = malloc(size);
        if (q) memcpy(q, p, size);
        return q;
    }
    count();
    return real_realloc(p, size);
}
int posix_memalign(void **out, size_t align, size_t size) { resolve(); count(); return real_posix_memalign(out, align, size); }
void *aligned_alloc(size_t align, size_t size) { resolve(); count(); return real_aligned_alloc(align, size); }
void *memalign(size_t align, size_t size) { resolve(); count(); return real_memalign(align, size); }
void free(void *p)
{
    if ((char *)p >= arena && (char *)p < arena + sizeof arena) return;
    resolve();
    if (real_free) real_free(p);
}
