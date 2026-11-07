#include "event_trace.h"

#include <QAbstractNativeEventFilter>
#include <QCoreApplication>
#include <QElapsedTimer>
#include <QEvent>
#include <QHash>
#include <QMetaObject>
#include <QTimer>

#include <algorithm>
#include <cstdio>
#include <vector>

#ifdef Q_OS_WIN
#include <windows.h>
#endif

namespace decibell {

namespace {

class EventTrace : public QObject, public QAbstractNativeEventFilter
{
public:
    explicit EventTrace(QByteArray file) : m_file(std::move(file))
    {
        m_clock.start();
        connect(&m_timer, &QTimer::timeout, this, &EventTrace::dump);
        m_timer.start(5000);
    }

    bool eventFilter(QObject *o, QEvent *e) override
    {
        const QString key = QStringLiteral("qt %1 -> %2").arg(int(e->type())).arg(QLatin1StringView(o->metaObject()->className()));
        ++m_counts[key];
        return false;
    }

    bool nativeEventFilter(const QByteArray &, void *message, qintptr *) override
    {
#ifdef Q_OS_WIN
        const auto *msg = static_cast<const MSG *>(message);
        ++m_counts[QStringLiteral("win 0x%1").arg(msg->message, 4, 16, QLatin1Char('0'))];
#endif
        return false;
    }

private:
    void dump()
    {
        std::vector<std::pair<qint64, QString>> top;
        for (auto it = m_counts.cbegin(); it != m_counts.cend(); ++it)
            top.emplace_back(it.value(), it.key());
        std::sort(top.begin(), top.end(), [](auto &a, auto &b) { return a.first > b.first; });
        const double secs = m_clock.restart() / 1000.0;
        if (FILE *f = fopen(m_file.constData(), "ab")) {
            fprintf(f, "--- %.1f s\n", secs);
            for (size_t i = 0; i < std::min<size_t>(top.size(), 15); ++i)
                fprintf(f, "%8.1f/s  %s\n", top[i].first / secs, qPrintable(top[i].second));
            fclose(f);
        }
        m_counts.clear();
    }

    QByteArray m_file;
    QHash<QString, qint64> m_counts;
    QElapsedTimer m_clock;
    QTimer m_timer;
};

} // namespace

void installEventTrace()
{
    const QByteArray file = qgetenv("DECIBELL_EVENT_TRACE");
    if (file.isEmpty())
        return;
    auto *trace = new EventTrace(file);
    QCoreApplication::instance()->installEventFilter(trace);
    QCoreApplication::instance()->installNativeEventFilter(trace);
}

} // namespace decibell
