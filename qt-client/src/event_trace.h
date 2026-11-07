// Debug aid (DECIBELL_EVENT_TRACE=<file>): what wakes the GUI thread. Counts the Qt events delivered on it (by type
// and receiver class) and the native Windows messages it takes, and appends the top entries per 5 s to the file.
#pragma once

namespace decibell {
void installEventTrace();
}
