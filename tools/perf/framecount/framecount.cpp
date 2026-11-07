// framecount: which video frames a window actually put on screen, measured the same way for any client.
//
// It watches the desktop through DXGI Desktop Duplication (every composition DWM makes) and reads the frame number
// that the test streams (qt-client/tools/streamgen) carry as a barcode: 16 cells across the top 1/18 of the picture,
// most significant bit first, white for 1. Each tile of the window is read on its own, so N streams side by side are
// N independent sequences. Per tile: frames displayed, frames never displayed, and the time between new frames.
//
//   framecount <window title substring> <seconds> [--fps=120] [--tiles=1] [--aspect=16:9] [--margin=px]
//
// Layout (both clients): one tile fills the client area; with more, tiles sit side by side with `margin` (default 8)
// around and between them; the video is letterboxed into its tile (object-fit: contain).
// The window is made topmost while measuring (an occluded window may not render at all). Prints one JSON line.
#include <windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <wrl/client.h>

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cwchar>
#include <string>
#include <vector>

using Microsoft::WRL::ComPtr;

namespace {

struct Found {
    std::wstring needle;
    HWND hwnd = nullptr;
};

BOOL CALLBACK findWindow(HWND hwnd, LPARAM lp)
{
    auto *f = reinterpret_cast<Found *>(lp);
    if (!IsWindowVisible(hwnd))
        return TRUE;
    wchar_t title[512];
    GetWindowTextW(hwnd, title, 512);
    if (wcsstr(title, f->needle.c_str())) {
        f->hwnd = hwnd;
        return FALSE;
    }
    return TRUE;
}

double pct(std::vector<double> v, double p)
{
    if (v.empty())
        return 0;
    std::sort(v.begin(), v.end());
    return v[std::min(v.size() - 1, size_t(p * double(v.size())))];
}

const wchar_t *option(int argc, wchar_t **argv, const wchar_t *name)
{
    const size_t n = wcslen(name);
    for (int i = 3; i < argc; ++i) {
        if (wcsncmp(argv[i], name, n) == 0 && argv[i][n] == L'=')
            return argv[i] + n + 1;
    }
    return nullptr;
}

struct Tile {
    // The barcode row in output coordinates.
    int x = 0, y = 0, w = 0;
    ComPtr<ID3D11Texture2D> staging;
    int last = -1;
    LONGLONG lastNewAt = 0;
    long long shown = 0, skipped = 0, backwards = 0, unreadable = 0;
    std::vector<double> intervals;
};

} // namespace

int wmain(int argc, wchar_t **argv)
{
    if (argc < 3) {
        fprintf(stderr, "usage: framecount <window title substring> <seconds> [--fps=120] [--tiles=1] [--aspect=16:9] "
                        "[--margin=px]\n");
        return 2;
    }
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    Found found{argv[1]};
    EnumWindows(findWindow, LPARAM(&found));
    if (!found.hwnd) {
        fwprintf(stderr, L"no visible window with \"%s\" in its title\n", argv[1]);
        return 1;
    }
    const double seconds = _wtof(argv[2]);
    const wchar_t *o;
    const double fps = (o = option(argc, argv, L"--fps")) ? _wtof(o) : 120;
    const int tiles = (o = option(argc, argv, L"--tiles")) ? _wtoi(o) : 1;
    double aspect = 16.0 / 9.0;
    if ((o = option(argc, argv, L"--aspect"))) {
        int aw = 16, ah = 9;
        swscanf_s(o, L"%d:%d", &aw, &ah);
        aspect = double(aw) / ah;
    }
    const int margin = (o = option(argc, argv, L"--margin")) ? _wtoi(o) : (tiles > 1 ? 8 : 0);
    SetWindowPos(found.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);

    RECT client;
    GetClientRect(found.hwnd, &client);
    POINT origin{0, 0};
    ClientToScreen(found.hwnd, &origin);
    const int cw = client.right, ch = client.bottom;
    const POINT center{origin.x + cw / 2, origin.y + ch / 2};

    // The output (monitor) showing the window's centre, and its adapter.
    ComPtr<IDXGIFactory1> factory;
    CreateDXGIFactory1(IID_PPV_ARGS(&factory));
    ComPtr<IDXGIAdapter1> adapter;
    ComPtr<IDXGIOutput1> output;
    DXGI_OUTPUT_DESC od{};
    for (UINT a = 0; !output && factory->EnumAdapters1(a, &adapter) != DXGI_ERROR_NOT_FOUND; ++a) {
        ComPtr<IDXGIOutput> out;
        for (UINT i = 0; adapter->EnumOutputs(i, &out) != DXGI_ERROR_NOT_FOUND; ++i) {
            out->GetDesc(&od);
            if (PtInRect(&od.DesktopCoordinates, center)) {
                out.As(&output);
                break;
            }
        }
    }
    if (!output) {
        fprintf(stderr, "no output shows the window\n");
        return 1;
    }
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> ctx;
    if (FAILED(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, nullptr, 0, D3D11_SDK_VERSION,
                                 &device, nullptr, &ctx))) {
        fprintf(stderr, "D3D11CreateDevice failed\n");
        return 1;
    }
    ComPtr<IDXGIOutputDuplication> dupl;
    if (HRESULT hr = output->DuplicateOutput(device.Get(), &dupl); FAILED(hr)) {
        fprintf(stderr, "DuplicateOutput failed (0x%08lx)\n", hr);
        return 1;
    }

    std::vector<Tile> t(size_t(std::max(1, tiles)));
    const double tileW = (cw - 2.0 * margin - (tiles - 1) * double(margin)) / tiles;
    const double tileH = ch - 2.0 * margin;
    for (int i = 0; i < tiles; ++i) {
        const double tx = margin + i * (tileW + margin), ty = margin;
        const double vw = std::min(tileW, tileH * aspect), vh = vw / aspect;
        const double vx = tx + (tileW - vw) / 2, vy = ty + (tileH - vh) / 2;
        t[i].x = origin.x - od.DesktopCoordinates.left + int(std::lround(vx));
        t[i].y = origin.y - od.DesktopCoordinates.top + int(std::lround(vy + vh / 36)); // the band's middle row
        t[i].w = int(vw);
        D3D11_TEXTURE2D_DESC td{};
        td.Width = UINT(t[i].w);
        td.Height = 1;
        td.MipLevels = 1;
        td.ArraySize = 1;
        td.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        td.SampleDesc.Count = 1;
        td.Usage = D3D11_USAGE_STAGING;
        td.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        device->CreateTexture2D(&td, nullptr, &t[i].staging);
    }

    LARGE_INTEGER freq, start, now;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&start);
    const LONGLONG end = start.QuadPart + LONGLONG(seconds * double(freq.QuadPart));
    long long compositions = 0;
    for (;;) {
        QueryPerformanceCounter(&now);
        if (now.QuadPart >= end)
            break;
        DXGI_OUTDUPL_FRAME_INFO info{};
        ComPtr<IDXGIResource> res;
        HRESULT hr = dupl->AcquireNextFrame(100, &info, &res);
        if (hr == DXGI_ERROR_WAIT_TIMEOUT)
            continue;
        if (FAILED(hr)) {
            fprintf(stderr, "AcquireNextFrame failed (0x%08lx)\n", hr);
            break;
        }
        if (info.LastPresentTime.QuadPart != 0) {
            ++compositions;
            ComPtr<ID3D11Texture2D> frame;
            res.As(&frame);
            for (Tile &tile : t) {
                const D3D11_BOX box{UINT(tile.x), UINT(tile.y), 0, UINT(tile.x + tile.w), UINT(tile.y + 1), 1};
                ctx->CopySubresourceRegion(tile.staging.Get(), 0, 0, 0, 0, frame.Get(), 0, &box);
            }
            for (Tile &tile : t) {
                D3D11_MAPPED_SUBRESOURCE m;
                if (FAILED(ctx->Map(tile.staging.Get(), 0, D3D11_MAP_READ, 0, &m)))
                    continue;
                const auto *px = static_cast<const uint8_t *>(m.pData);
                int value = 0;
                bool clean = true;
                for (int bit = 0; bit < 16; ++bit) {
                    const int x = int((bit + 0.5) * tile.w / 16);
                    const int luma = (px[x * 4] + px[x * 4 + 1] + px[x * 4 + 2]) / 3;
                    if (luma > 80 && luma < 170)
                        clean = false; // mid-grey: not our barcode (the video isn't there yet, or scaled oddly)
                    value = (value << 1) | (luma >= 128 ? 1 : 0);
                }
                ctx->Unmap(tile.staging.Get(), 0);
                if (!clean) {
                    ++tile.unreadable;
                    continue;
                }
                if (value == tile.last)
                    continue;
                if (tile.last >= 0) {
                    ++tile.shown;
                    if (value > tile.last)
                        tile.skipped += value - tile.last - 1;
                    else
                        ++tile.backwards; // the file looped (or a frame went back: counted, not judged)
                    tile.intervals.push_back(double(info.LastPresentTime.QuadPart - tile.lastNewAt) * 1000.0
                                             / double(freq.QuadPart));
                }
                tile.last = value;
                tile.lastNewAt = info.LastPresentTime.QuadPart;
            }
        }
        dupl->ReleaseFrame();
    }
    SetWindowPos(found.hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);

    const double period = 1000.0 / fps;
    printf("{\"seconds\":%.1f,\"fps\":%.0f,\"compositions\":%lld,\"tiles\":[", seconds, fps, compositions);
    for (size_t i = 0; i < t.size(); ++i) {
        const Tile &tile = t[i];
        const long long late =
            std::count_if(tile.intervals.begin(), tile.intervals.end(), [&](double d) { return d > period * 1.5; });
        printf("%s{\"shown\":%lld,\"shown_per_s\":%.1f,\"skipped\":%lld,\"backwards\":%lld,\"unreadable\":%lld,"
               "\"interval_ms\":{\"p50\":%.2f,\"p95\":%.2f,\"p99\":%.2f,\"max\":%.2f},\"late\":%lld}",
               i ? "," : "", tile.shown, double(tile.shown) / seconds, tile.skipped, tile.backwards, tile.unreadable,
               pct(tile.intervals, 0.5), pct(tile.intervals, 0.95), pct(tile.intervals, 0.99), pct(tile.intervals, 1.0),
               late);
    }
    printf("]}\n");
    return 0;
}
