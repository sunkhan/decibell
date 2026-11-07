// A watched stream on screen (Windows, D3D11). The decoder's pictures are NV12 slices of a D3D11 texture array; at
// each scene-graph sync the newest one goes through the D3D11 video processor (colour conversion + scaling in one
// GPU pass) into an RGBA texture the size the item is drawn at, never larger than the video: what Electron's
// player does with its canvas backing store. The scene graph draws that texture with a plain image node.
#pragma once

#include <QPointer>
#include <QQuickItem>
#include <QSize>
#include <QTimer>
#include <QtQml/qqmlregistration.h>

#include <atomic>
#include <memory>

namespace decibell::video {

class StreamPlayer;

class VideoItem : public QQuickItem
{
    Q_OBJECT
    QML_ELEMENT
    // Spike: a .dbv test stream (tools/streamgen). The client will take a stream id here.
    Q_PROPERTY(QString source READ source WRITE setSource NOTIFY sourceChanged)
    Q_PROPERTY(QString codecName READ codecName NOTIFY statsChanged)
    Q_PROPERTY(QSize videoSize READ videoSize NOTIFY statsChanged)
    Q_PROPERTY(int videoFps READ videoFps NOTIFY statsChanged)
    // Over the last second.
    Q_PROPERTY(int decodedFps READ decodedFps NOTIFY statsChanged)
    Q_PROPERTY(int presentedFps READ presentedFps NOTIFY statsChanged)
    Q_PROPERTY(QString error READ error NOTIFY statsChanged)

public:
    explicit VideoItem(QQuickItem *parent = nullptr);
    ~VideoItem() override;

    QString source() const { return m_source; }
    void setSource(const QString &s);
    QString codecName() const;
    QSize videoSize() const;
    int videoFps() const;
    int decodedFps() const { return m_decodedFps; }
    int presentedFps() const { return m_presentedFps; }
    QString error() const { return m_error; }

    // --bench-video: start recording, then a JSON array with one object per stream on screen.
    static void benchStart();
    static QString benchReport();

signals:
    void sourceChanged();
    void statsChanged();

protected:
    QSGNode *updatePaintNode(QSGNode *old, UpdatePaintNodeData *) override;
    void geometryChange(const QRectF &newGeometry, const QRectF &oldGeometry) override;

private:
    void sampleStats();

    QString m_source;
    QString m_error;
    std::unique_ptr<StreamPlayer> m_player; // created on the render thread at the first sync (needs the device)
    bool m_playerFailed = false;
    std::atomic<uint64_t> m_presented{0};
    QTimer m_statsTimer;
    uint64_t m_lastDecoded = 0, m_lastPresented = 0;
    int m_decodedFps = 0, m_presentedFps = 0;
    // Bench window baselines.
    uint64_t m_benchDecoded = 0, m_benchPresented = 0, m_benchReplaced = 0, m_benchFed = 0;
};

} // namespace decibell::video
