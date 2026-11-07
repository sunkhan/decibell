#include "frame_stats.h"

#include <QQuickWindow>
#include <QScreen>
#include <algorithm>

#ifdef Q_OS_WIN
#include <windows.h>
#endif

namespace decibell {

namespace {

double processCpuMs()
{
#ifdef Q_OS_WIN
    FILETIME created, exited, kernel, user;
    if (!GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user))
        return 0;
    auto ms = [](const FILETIME &f) { return double((quint64(f.dwHighDateTime) << 32) | f.dwLowDateTime) / 10000.0; };
    return ms(kernel) + ms(user);
#else
    return 0;
#endif
}

double percentile(std::vector<double> v, double p)
{
    if (v.empty())
        return 0;
    std::sort(v.begin(), v.end());
    return v[size_t(std::min<double>(v.size() - 1, std::floor((v.size() - 1) * p)))];
}

} // namespace

FrameStats::FrameStats(QQuickWindow *window) : QObject(window), m_window(window)
{
    m_swapsNs.reserve(1 << 16);
    m_stages.reserve(1 << 16);
    m_clock.start();
    auto mark = [this](int i) {
        return [this, i] {
            std::lock_guard g(m_lock);
            if (!m_recording)
                return;
            m_current.t[i] = m_clock.nsecsElapsed();
            if (i == 5 && m_stages.size() < m_stages.capacity()) {
                m_stages.push_back(m_current);
                m_current = {};
            }
        };
    };
    connect(window, &QQuickWindow::beforeFrameBegin, this, mark(0), Qt::DirectConnection);
    connect(window, &QQuickWindow::beforeSynchronizing, this, mark(1), Qt::DirectConnection);
    connect(window, &QQuickWindow::afterSynchronizing, this, mark(2), Qt::DirectConnection);
    connect(window, &QQuickWindow::beforeRendering, this, mark(3), Qt::DirectConnection);
    connect(window, &QQuickWindow::afterRendering, this, mark(4), Qt::DirectConnection);
    connect(window, &QQuickWindow::afterFrameEnd, this, mark(5), Qt::DirectConnection);
    // frameSwapped is emitted on the render thread right after Present.
    connect(window, &QQuickWindow::frameSwapped, this, [this] {
        std::lock_guard g(m_lock);
        if (m_recording && m_swapsNs.size() < m_swapsNs.capacity())
            m_swapsNs.push_back(m_clock.nsecsElapsed());
    }, Qt::DirectConnection);
}

void FrameStats::start()
{
    std::lock_guard g(m_lock);
    m_swapsNs.clear();
    m_stages.clear();
    m_current = {};
    m_recording = true;
    m_cpuStartMs = processCpuMs();
}

QString FrameStats::report() const
{
    std::vector<qint64> swaps;
    std::vector<Stages> stages;
    {
        std::lock_guard g(m_lock);
        swaps = m_swapsNs;
        stages = m_stages;
    }
    // Stage durations (ms): beginFrame (waiting for a swap-chain buffer), begin->sync (GUI handing over),
    // sync, sync->render, render, render->end (endFrame = Present).
    QString stageJson;
    const char *names[] = {"begin_frame", "to_sync", "sync", "to_render", "render", "present"};
    const int from[] = {0, 0, 1, 2, 3, 4};
    const int to[] = {1, 1, 2, 3, 4, 5};
    for (int s = 0; s < 6; ++s) {
        std::vector<double> d;
        for (const auto &f : stages)
            if (f.t[from[s]] && f.t[to[s]])
                d.push_back((f.t[to[s]] - f.t[from[s]]) / 1e6);
        stageJson += QStringLiteral(",\"%1\":{\"p50\":%2,\"p95\":%3,\"p99\":%4}")
                         .arg(QLatin1StringView(names[s]))
                         .arg(percentile(d, 0.5), 0, 'f', 3)
                         .arg(percentile(d, 0.95), 0, 'f', 3)
                         .arg(percentile(d, 0.99), 0, 'f', 3);
    }
    // The render thread idle between one frame's end and the next frame's begin: waiting for the GUI thread
    // (its update request, animations and polish) to hand over the next frame.
    std::vector<double> gap;
    for (size_t i = 1; i < stages.size(); ++i)
        if (stages[i].t[0] && stages[i - 1].t[5])
            gap.push_back((stages[i].t[0] - stages[i - 1].t[5]) / 1e6);
    stageJson += QStringLiteral(",\"gap\":{\"p50\":%1,\"p95\":%2,\"p99\":%3}")
                     .arg(percentile(gap, 0.5), 0, 'f', 3)
                     .arg(percentile(gap, 0.95), 0, 'f', 3)
                     .arg(percentile(gap, 0.99), 0, 'f', 3);
    const double hz = m_window->screen() ? m_window->screen()->refreshRate() : 60.0;
    const double periodMs = 1000.0 / hz;
    std::vector<double> intervals;
    for (size_t i = 1; i < swaps.size(); ++i)
        intervals.push_back((swaps[i] - swaps[i - 1]) / 1e6);
    const auto missed = std::count_if(intervals.begin(), intervals.end(), [&](double d) { return d > periodMs * 1.5; });
    const double spanMs = swaps.size() > 1 ? (swaps.back() - swaps.front()) / 1e6 : 0;
    const double cpuMs = processCpuMs() - m_cpuStartMs;
    return QStringLiteral(
               "{\"frames\":%1,\"refresh_hz\":%2,\"span_ms\":%3,\"interval_ms\":{\"p50\":%4,\"p95\":%5,\"p99\":%6,\"max\":%7},"
               "\"missed_vsyncs\":%8,\"cpu_ms_per_frame\":%9,\"cpu_core_pct\":%10%11}")
        .arg(swaps.size())
        .arg(hz, 0, 'f', 1)
        .arg(spanMs, 0, 'f', 0)
        .arg(percentile(intervals, 0.5), 0, 'f', 2)
        .arg(percentile(intervals, 0.95), 0, 'f', 2)
        .arg(percentile(intervals, 0.99), 0, 'f', 2)
        .arg(percentile(intervals, 1.0), 0, 'f', 2)
        .arg(missed)
        .arg(swaps.size() ? cpuMs / swaps.size() : 0, 0, 'f', 3)
        .arg(spanMs > 0 ? 100.0 * cpuMs / spanMs : 0, 0, 'f', 1)
        .arg(stageJson);
}

} // namespace decibell
