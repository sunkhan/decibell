// Frame pacing for benchmarks: when each frame was presented (render thread), and the process's CPU over the run.
#pragma once

#include <QElapsedTimer>
#include <QObject>
#include <mutex>
#include <vector>

class QQuickWindow;

namespace decibell {

class FrameStats : public QObject
{
public:
    explicit FrameStats(QQuickWindow *window);
    // Starts recording now (frames before are ignored).
    void start();
    // One line of JSON: frames, refresh rate, presented-interval percentiles, frames over 1.5 refresh
    // intervals (missed vsyncs), and CPU ms per frame / % of one core over the run.
    QString report() const;

private:
    QQuickWindow *m_window;
    QElapsedTimer m_clock;
    mutable std::mutex m_lock;
    std::vector<qint64> m_swapsNs; // reserved up front: no allocation on the render thread while recording
    // Per-frame stage timestamps on the render thread: frame begin (before beginFrame), sync start / end,
    // render start / end, frame end (after endFrame = Present).
    struct Stages { qint64 t[6] = {}; };
    std::vector<Stages> m_stages;
    Stages m_current;
    bool m_recording = false;
    double m_cpuStartMs = 0;
};

} // namespace decibell
