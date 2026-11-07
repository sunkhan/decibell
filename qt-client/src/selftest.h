// Spike self-tests that run in the hidden window (nothing shows on screen): synthetic input through the real event
// delivery, results printed as JSON lines (stdout and DECIBELL_BENCH_OUT), an optional capture of the window.
//   --selftest selection [--capture out.png]: drags across messages, double-clicks a word, copies.
//   --selftest history --page-size N: older pages and new messages must not move what is being read.
//   --selftest hold: renders once and stays up DECIBELL_HOLD_SECONDS (default 30), for memory measurements.
//   --selftest regrab --capture out.png: three renders through new devices (as after a lost device), the last saved.
#pragma once

#include <QString>

class QQuickWindow;

namespace decibell {
// Runs the named test once the window has laid out; quits the app with 0 (pass) or 1.
void runSelfTest(QQuickWindow *window, const QString &name, const QString &capture);
}
