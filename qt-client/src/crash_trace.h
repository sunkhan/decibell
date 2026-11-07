#pragma once
#include <QtGlobal>

namespace decibell {
// Dev aid: print a symbolised stack on abort / crash / std::terminate (crash_trace.cpp).
void installCrashTrace();
} // namespace decibell
