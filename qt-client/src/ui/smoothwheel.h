// Mouse-wheel scrolling as Chromium does it on Windows, measured on the Electron client with real WM_MOUSEWHEEL
// messages (tools/perf/electron-wheel.mjs): a notch moves 100 px along a smoothstep curve over ~175 ms; a notch
// arriving mid-animation adds 100 px to the target and restarts from the current position keeping the current
// speed (a cubic Hermite from (position, velocity) to (target, 0), which is smoothstep from rest). Touchpads
// (pixel deltas) scroll by their own deltas, unanimated. Replaces the Flickable's own wheel handling.
#pragma once

#include <QElapsedTimer>
#include <QObject>
#include <QPointer>
#include <QQuickItem>
#include <QVariantAnimation>
#include <QtQml/qqmlregistration.h>

namespace decibell {

class SmoothWheel : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    // The Flickable / ListView to drive (its contentY).
    Q_PROPERTY(QQuickItem *target READ target WRITE setTarget NOTIFY targetChanged)
    // +1 (default): wheel up lowers contentY, i.e. shows what is above, in a top-to-bottom list and in a
    // bottom-to-top ListView alike (its older rows sit at lower y). -1 inverts.
    Q_PROPERTY(int direction MEMBER m_direction NOTIFY targetChanged)

public:
    explicit SmoothWheel(QObject *parent = nullptr);
    QQuickItem *target() const { return m_target; }
    void setTarget(QQuickItem *t);

signals:
    void targetChanged();

protected:
    bool eventFilter(QObject *watched, QEvent *event) override;

private:
    void tick();
    qreal clamp(qreal y) const;

    QPointer<QQuickItem> m_target;
    int m_direction = 1;
    QVariantAnimation m_anim;
    QElapsedTimer m_clock;   // trace timestamps
    QElapsedTimer m_segment; // wall time since the current segment started
    // Current segment: from (p0, v0 px/ms) at its start to (p1, 0) over m_durationMs.
    qreal m_p0 = 0, m_v0 = 0, m_p1 = 0;
    qreal m_durationMs = 175;
    qreal m_lastPos = 0, m_lastVel = 0;
};

} // namespace decibell
