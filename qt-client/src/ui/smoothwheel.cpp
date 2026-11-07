#include "smoothwheel.h"

#include <QWheelEvent>
#include <cstdio>
#include <QtMath>

namespace decibell {

namespace {
constexpr qreal kPixelsPerNotch = 100;  // 120 units = WHEEL_DELTA / 120 × 3 lines × 100/3 px (Chromium, Windows)
constexpr qreal kDurationMs = 175;      // measured: 172–182 ms per notch
}

SmoothWheel::SmoothWheel(QObject *parent) : QObject(parent)
{
    m_anim.setStartValue(0.0);
    m_anim.setEndValue(1.0);
    // The animation is only a per-frame tick (it advances with the scene graph's frames); the curve runs on wall
    // time. Qt's animation driver steps time by one assumed vsync per rendered frame, so whenever frames come
    // slower than the refresh rate (the first frames after idle, while clocks ramp up) its time lags: measured,
    // a 175 ms notch took 196 ms on the animation's own clock.
    m_anim.setDuration(int(kDurationMs) * 4);
    m_anim.setEasingCurve(QEasingCurve::Linear);
    connect(&m_anim, &QVariantAnimation::valueChanged, this, [this] { tick(); });
}

void SmoothWheel::setTarget(QQuickItem *t)
{
    if (m_target == t)
        return;
    if (m_target)
        m_target->removeEventFilter(this);
    m_target = t;
    if (m_target)
        m_target->installEventFilter(this);
    emit targetChanged();
}

qreal SmoothWheel::clamp(qreal y) const
{
    const qreal origin = m_target->property("originY").toReal();
    const qreal content = m_target->property("contentHeight").toReal();
    const qreal h = m_target->height();
    return qBound(origin, y, qMax(origin, origin + content - h));
}

void SmoothWheel::tick()
{
    if (!m_target)
        return;
    const qreal t01 = qMin<qreal>(1.0, m_segment.nsecsElapsed() / 1e6 / m_durationMs);
    // Cubic Hermite from (p0, v0) to (p1, 0) over T: smoothstep when v0 = 0.
    const qreal T = m_durationMs, t = t01;
    const qreal t2 = t * t, t3 = t2 * t;
    const qreal h00 = 2 * t3 - 3 * t2 + 1, h10 = t3 - 2 * t2 + t, h01 = -2 * t3 + 3 * t2;
    const qreal pos = h00 * m_p0 + h10 * T * m_v0 + h01 * m_p1;
    // Velocity (px/ms) for a retarget: d/dt of the above.
    const qreal d00 = 6 * t2 - 6 * t, d10 = 3 * t2 - 4 * t + 1, d01 = -6 * t2 + 6 * t;
    m_lastVel = (d00 * m_p0 + d10 * T * m_v0 + d01 * m_p1) / T;
    m_lastPos = pos;
    m_target->setProperty("contentY", clamp(pos));
    if (t01 >= 1.0)
        m_anim.stop();
    static const bool trace = qEnvironmentVariableIsSet("DECIBELL_WHEEL_TRACE");
    if (trace) {
        if (!m_clock.isValid())
            m_clock.start();
        fprintf(stderr, "wheel %.1f %.2f%c", m_clock.nsecsElapsed() / 1e6, pos, 10);
    }
}

bool SmoothWheel::eventFilter(QObject *watched, QEvent *event)
{
    if (watched != m_target || event->type() != QEvent::Wheel)
        return QObject::eventFilter(watched, event);
    auto *we = static_cast<QWheelEvent *>(event);
    const qreal current = m_target->property("contentY").toReal();
    if (!we->pixelDelta().isNull() || we->source() == Qt::MouseEventSynthesizedBySystem) {
        // Touchpad / precise device: follow the fingers, no animation.
        m_anim.stop();
        m_target->setProperty("contentY", clamp(current - m_direction * we->pixelDelta().y()));
        event->accept();
        return true;
    }
    const qreal notches = we->angleDelta().y() / 120.0;
    if (qFuzzyIsNull(notches))
        return QObject::eventFilter(watched, event);
    const bool running = m_anim.state() == QAbstractAnimation::Running;
    // Retarget from where we are, keeping the speed; from rest, start at the current position.
    m_p0 = running ? m_lastPos : current;
    m_v0 = running ? m_lastVel : 0;
    const qreal base = running ? m_p1 : current;
    m_p1 = clamp(base - m_direction * notches * kPixelsPerNotch);
    m_durationMs = kDurationMs;
    m_lastPos = m_p0;
    m_anim.stop();
    m_segment.start();
    m_anim.start();
    event->accept();
    return true;
}

} // namespace decibell
