// alloc-count: counts heap allocations per thread in an unmodified binary, and lets the binary ask for its own
// thread's count. `decibell --selftest video-fastpath` looks the function up at run time and, when this library is
// preloaded, holds the UI thread to its allocation budget per presented video frame:
//
//   cc -O2 -shared -fPIC crates/decibell-video/tools/alloc-count.c -o /tmp/alloc-count.so
//   LD_PRELOAD=/tmp/alloc-count.so target/release/decibell --selftest video-fastpath
//
// Counts malloc, calloc, realloc, posix_memalign, aligned_alloc and memalign of every caller on the thread: Rust
// code (the system allocator), wgpu, the GPU driver, libwayland. (tools/streamgen/malloc-count.c is the same
// counter with a per-thread dump on SIGUSR2, for measuring a running client from outside.)
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stddef.h>

static void *(*real_malloc)(size_t);
static void *(*real_calloc)(size_t, size_t);
static void *(*real_realloc)(void *, size_t);
static int (*real_posix_memalign)(void **, size_t, size_t);
static void *(*real_aligned_alloc)(size_t, size_t);
static void *(*real_memalign)(size_t, size_t);
static void (*real_free)(void *);
static __thread unsigned long count;

// dlsym allocates while the real functions are being resolved: that is served from a static arena.
static char arena[1 << 16];
static size_t arena_used;
static int resolving;

static void resolve(void)
{
    if (real_malloc || resolving)
        return;
    resolving = 1;
    real_calloc = dlsym(RTLD_NEXT, "calloc");
    real_realloc = dlsym(RTLD_NEXT, "realloc");
    real_posix_memalign = dlsym(RTLD_NEXT, "posix_memalign");
    real_aligned_alloc = dlsym(RTLD_NEXT, "aligned_alloc");
    real_memalign = dlsym(RTLD_NEXT, "memalign");
    real_free = dlsym(RTLD_NEXT, "free");
    real_malloc = dlsym(RTLD_NEXT, "malloc");
    resolving = 0;
}

static void *from_arena(size_t size)
{
    size = (size + 15) & ~(size_t)15;
    if (arena_used + size > sizeof arena)
        return NULL;
    void *block = arena + arena_used;
    arena_used += size;
    return block;
}

static int in_arena(void *block) { return (char *)block >= arena && (char *)block < arena + sizeof arena; }

// Allocations made by the calling thread since it started.
unsigned long decibell_alloc_count(void) { return count; }

void *malloc(size_t size)
{
    resolve();
    if (!real_malloc)
        return from_arena(size);
    count++;
    return real_malloc(size);
}

void *calloc(size_t n, size_t size)
{
    resolve();
    if (!real_calloc)
        return from_arena(n * size);     // the arena is zeroed and never reused
    count++;
    return real_calloc(n, size);
}

void *realloc(void *block, size_t size)
{
    resolve();
    if (in_arena(block) || !real_realloc) {
        void *moved = malloc(size);
        if (moved && block)
            __builtin_memcpy(moved, block, size);   // reads at most into the arena's own bytes
        return moved;
    }
    count++;
    return real_realloc(block, size);
}

int posix_memalign(void **out, size_t alignment, size_t size)
{
    resolve();
    count++;
    return real_posix_memalign(out, alignment, size);
}

void *aligned_alloc(size_t alignment, size_t size)
{
    resolve();
    count++;
    return real_aligned_alloc(alignment, size);
}

void *memalign(size_t alignment, size_t size)
{
    resolve();
    count++;
    return real_memalign(alignment, size);
}

void free(void *block)
{
    if (in_arena(block))
        return;
    resolve();
    if (real_free)
        real_free(block);
}
