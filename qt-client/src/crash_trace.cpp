// Dev aid: on an abort, a crash or an uncaught exception, print the stack with symbol names (DbgHelp) to stderr
// and to DECIBELL_CRASH_TRACE (a file) when set. Needs the PDB beside the exe (RelWithDebInfo / Debug builds).
// Crash reports for users come later (design doc §10: sentry-native).
#include "crash_trace.h"

#ifdef Q_OS_WIN
#include <windows.h>
#include <dbghelp.h>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <exception>

namespace {

void writeTrace(const char *why)
{
    HANDLE proc = GetCurrentProcess();
    SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES);
    SymInitialize(proc, nullptr, TRUE);
    void *frames[64];
    const USHORT n = CaptureStackBackTrace(1, 64, frames, nullptr);
    char buf[sizeof(SYMBOL_INFO) + 512];
    auto *sym = reinterpret_cast<SYMBOL_INFO *>(buf);
    FILE *files[2] = {stderr, nullptr};
    if (const char *path = getenv("DECIBELL_CRASH_TRACE"))
        fopen_s(&files[1], path, "w");
    for (FILE *f : files) {
        if (!f)
            continue;
        fprintf(f, "decibell-qt: %s\n", why);
        for (USHORT i = 0; i < n; ++i) {
            memset(buf, 0, sizeof(buf));
            sym->SizeOfStruct = sizeof(SYMBOL_INFO);
            sym->MaxNameLen = 511;
            DWORD64 disp = 0;
            IMAGEHLP_LINE64 line{};
            line.SizeOfStruct = sizeof(line);
            DWORD ldisp = 0;
            const DWORD64 addr = reinterpret_cast<DWORD64>(frames[i]);
            const bool named = SymFromAddr(proc, addr, &disp, sym);
            const bool lined = SymGetLineFromAddr64(proc, addr, &ldisp, &line);
            fprintf(f, "  #%-2u %s+0x%llx %s:%lu\n", i, named ? sym->Name : "?", (unsigned long long)disp,
                    lined ? line.FileName : "", lined ? line.LineNumber : 0ul);
        }
        fflush(f);
    }
    if (files[1])
        fclose(files[1]);
}

LONG WINAPI onException(EXCEPTION_POINTERS *e)
{
    char why[64];
    sprintf_s(why, "exception 0x%08lx", e->ExceptionRecord->ExceptionCode);
    writeTrace(why);
    return EXCEPTION_CONTINUE_SEARCH;
}

} // namespace

namespace decibell {
void installCrashTrace()
{
    SetUnhandledExceptionFilter(onException);
    signal(SIGABRT, [](int) { writeTrace("abort"); });
    std::set_terminate([] { writeTrace("std::terminate"); std::abort(); });
    _set_abort_behavior(0, _WRITE_ABORT_MSG | _CALL_REPORTFAULT); // let our SIGABRT handler run first
}
} // namespace decibell

#else
namespace decibell { void installCrashTrace() {} }
#endif
