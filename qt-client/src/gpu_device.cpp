#include "gpu_device.h"

#include <QQuickGraphicsDevice>
#include <QQuickWindow>
#include <QtGlobal>

#ifdef Q_OS_WIN
#include <d3d11_4.h>
#include <dxgi1_6.h>

namespace decibell {

bool installGraphicsDevice(QQuickWindow *window)
{
    if (!window || qEnvironmentVariableIsSet("QSG_RHI_BACKEND"))
        return false; // an explicit backend choice (testing) wins

    UINT flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    if (qEnvironmentVariableIntValue("DECIBELL_D3D_DRIVER_THREADS") != 1)
        flags |= D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS;

    const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0};
    ID3D11Device *device = nullptr;
    ID3D11DeviceContext *context = nullptr;
    D3D_FEATURE_LEVEL got{};
    HRESULT hr = D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr, flags, levels, UINT(std::size(levels)),
                                   D3D11_SDK_VERSION, &device, &got, &context);
    if (FAILED(hr)) {
        qWarning("D3D11CreateDevice failed (0x%08lx); Qt creates its own device", hr);
        return false;
    }
    // The decoder thread and Qt's render thread share the immediate context (mpv / Chromium model).
    ID3D11Multithread *mt = nullptr;
    if (SUCCEEDED(context->QueryInterface(__uuidof(ID3D11Multithread), reinterpret_cast<void **>(&mt)))) {
        mt->SetMultithreadProtected(TRUE);
        mt->Release();
    }
    window->setGraphicsDevice(QQuickGraphicsDevice::fromDeviceAndContext(device, context));
    // Qt holds its own references for the window's lifetime; the device must outlive it, so ours are kept
    // (released at process exit).
    return true;
}

} // namespace decibell

#else

namespace decibell {
bool installGraphicsDevice(QQuickWindow *) { return false; }
} // namespace decibell

#endif
