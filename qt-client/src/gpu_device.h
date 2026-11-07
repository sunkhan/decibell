// The client's own graphics device, handed to Qt Quick before the window is exposed (design doc §8.2).
#pragma once

class QQuickWindow;

namespace decibell {

// Windows: creates a D3D11 device on the default hardware adapter with BGRA + VIDEO support and multithread
// protection (the video decoder submits from its own thread), and gives it to `window`. Unless
// DECIBELL_D3D_DRIVER_THREADS=1, it asks the driver not to start worker threads
// (D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS): measured, NVIDIA's D3D11 driver otherwise wakes
// one of them ~80 times a second at idle. Returns false (Qt makes its own device) when anything fails.
// Elsewhere a no-op returning false.
bool installGraphicsDevice(QQuickWindow *window);

} // namespace decibell
