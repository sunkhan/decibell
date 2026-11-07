#include "videoitem.h"

#include "streamplayer.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QQuickWindow>
#include <QSGImageNode>
#include <QSGRendererInterface>
#include <QtQuick/qsgtexture_platform.h>

#include <algorithm>
#include <chrono>
#include <unordered_map>

#include <d3d11_4.h>
#include <wrl/client.h>

extern "C" {
#include <libavutil/frame.h>
#include <libavutil/pixfmt.h>
}

using Microsoft::WRL::ComPtr;

namespace decibell::video {

namespace {

QList<VideoItem *> &liveItems()
{
    static QList<VideoItem *> items;
    return items;
}

int64_t nowNs()
{
    return std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now().time_since_epoch())
        .count();
}

// The video's rectangle inside `bounds`, letterboxed (object-fit: contain).
QRectF fitRect(const QRectF &bounds, QSize video)
{
    if (video.isEmpty() || bounds.isEmpty())
        return {};
    const qreal s = std::min(bounds.width() / video.width(), bounds.height() / video.height());
    const QSizeF size(video.width() * s, video.height() * s);
    return QRectF(bounds.x() + (bounds.width() - size.width()) / 2, bounds.y() + (bounds.height() - size.height()) / 2,
                  size.width(), size.height());
}

DXGI_COLOR_SPACE_TYPE inputColorSpace(const AVFrame *f)
{
    const bool full = f->color_range == AVCOL_RANGE_JPEG;
    const bool bt601 = f->colorspace == AVCOL_SPC_BT470BG || f->colorspace == AVCOL_SPC_SMPTE170M;
    // Unspecified is BT.709, as Chromium treats an HD stream.
    if (bt601)
        return full ? DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601 : DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601;
    return full ? DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709 : DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709;
}

// Render-thread state: the video processor and the output texture, released with the node (on the render thread).
class VideoNode : public QSGNode
{
public:
    explicit VideoNode(QQuickWindow *window) : m_window(window)
    {
        QSGRendererInterface *ri = window->rendererInterface();
        auto *device = static_cast<ID3D11Device *>(ri->getResource(window, QSGRendererInterface::DeviceResource));
        auto *context =
            static_cast<ID3D11DeviceContext *>(ri->getResource(window, QSGRendererInterface::DeviceContextResource));
        m_device = device;
        if (device)
            device->QueryInterface(IID_PPV_ARGS(m_videoDevice.GetAddressOf()));
        if (context) {
            context->QueryInterface(IID_PPV_ARGS(m_videoContext.GetAddressOf()));
            context->QueryInterface(IID_PPV_ARGS(m_mt.GetAddressOf()));
        }
    }
    ~VideoNode() override
    {
        if (m_image) {
            removeChildNode(m_image);
            delete m_image;
        }
        delete m_texture;
    }

    // Converts `frame` into the output texture at `out` pixels. False when the GPU path is unavailable.
    bool present(const AVFrame *frame, QSize out)
    {
        if (!m_videoDevice || !m_videoContext || !m_mt || frame->format != AV_PIX_FMT_D3D11)
            return false;
        auto *tex = reinterpret_cast<ID3D11Texture2D *>(frame->data[0]);
        const auto slice = UINT(reinterpret_cast<intptr_t>(frame->data[1]));
        const QSize in(frame->width, frame->height);
        if (in != m_in || out != m_out) {
            if (!createProcessor(in, out))
                return false;
        }
        if (tex != m_poolTexture || frame->hw_frames_ctx->data != m_pool) {
            // The decoder made a new pool (a new stream, a resolution change): its slices need new views.
            m_inputViews.clear();
            m_poolTexture = tex;
            m_pool = frame->hw_frames_ctx->data;
        }
        ComPtr<ID3D11VideoProcessorInputView> &view = m_inputViews[slice];
        if (!view) {
            D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC desc{};
            desc.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
            desc.Texture2D.ArraySlice = slice;
            if (FAILED(m_videoDevice->CreateVideoProcessorInputView(tex, m_enumerator.Get(), &desc, &view)))
                return false;
        }
        D3D11_VIDEO_PROCESSOR_STREAM stream{};
        stream.Enable = TRUE;
        stream.pInputSurface = view.Get();
        const RECT src{0, 0, in.width(), in.height()};
        const RECT dst{0, 0, out.width(), out.height()};
        m_mt->Enter();
        m_videoContext->VideoProcessorSetStreamSourceRect(m_processor.Get(), 0, TRUE, &src);
        m_videoContext->VideoProcessorSetStreamDestRect(m_processor.Get(), 0, TRUE, &dst);
        m_videoContext->VideoProcessorSetOutputTargetRect(m_processor.Get(), TRUE, &dst);
        m_videoContext->VideoProcessorSetStreamColorSpace1(m_processor.Get(), 0, inputColorSpace(frame));
        const HRESULT hr = m_videoContext->VideoProcessorBlt(m_processor.Get(), m_outputView.Get(), 0, 1, &stream);
        m_mt->Leave();
        return SUCCEEDED(hr);
    }

    void setRect(const QRectF &r)
    {
        if (m_image)
            m_image->setRect(r);
    }

private:
    bool createProcessor(QSize in, QSize out)
    {
        m_processor.Reset();
        m_enumerator.Reset();
        m_inputViews.clear();
        m_outputView.Reset();
        D3D11_VIDEO_PROCESSOR_CONTENT_DESC content{};
        content.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
        content.InputWidth = UINT(in.width());
        content.InputHeight = UINT(in.height());
        content.InputFrameRate = {60, 1};
        content.OutputWidth = UINT(out.width());
        content.OutputHeight = UINT(out.height());
        content.OutputFrameRate = {60, 1};
        content.Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;
        if (FAILED(m_videoDevice->CreateVideoProcessorEnumerator(&content, &m_enumerator)))
            return false;
        UINT support = 0;
        // RGBA, not BGRA: Qt wraps a native D3D11 texture as RGBA8 (QSGD3D11Texture::fromNative).
        if (FAILED(m_enumerator->CheckVideoProcessorFormat(DXGI_FORMAT_R8G8B8A8_UNORM, &support))
            || !(support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT)) {
            qWarning("video processor cannot write RGBA8");
            return false;
        }
        if (FAILED(m_videoDevice->CreateVideoProcessor(m_enumerator.Get(), 0, &m_processor)))
            return false;
        // No driver "enhancements" (denoise, edge enhancement, super resolution): the picture as encoded.
        m_videoContext->VideoProcessorSetStreamAutoProcessingMode(m_processor.Get(), 0, FALSE);
        m_videoContext->VideoProcessorSetStreamFrameFormat(m_processor.Get(), 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
        m_videoContext->VideoProcessorSetOutputColorSpace1(m_processor.Get(), DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);

        D3D11_TEXTURE2D_DESC td{};
        td.Width = UINT(out.width());
        td.Height = UINT(out.height());
        td.MipLevels = 1;
        td.ArraySize = 1;
        td.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        td.SampleDesc.Count = 1;
        td.Usage = D3D11_USAGE_DEFAULT;
        td.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
        ComPtr<ID3D11Texture2D> texture;
        if (FAILED(m_device->CreateTexture2D(&td, nullptr, &texture)))
            return false;
        D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC od{};
        od.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
        if (FAILED(m_videoDevice->CreateVideoProcessorOutputView(texture.Get(), m_enumerator.Get(), &od, &m_outputView)))
            return false;

        QSGTexture *sg = QNativeInterface::QSGD3D11Texture::fromNative(texture.Get(), m_window, out,
                                                                        QQuickWindow::TextureIsOpaque);
        if (!sg)
            return false;
        sg->setFiltering(QSGTexture::Linear);
        if (!m_image) {
            m_image = m_window->createImageNode();
            m_image->setOwnsTexture(false);
            m_image->setFiltering(QSGTexture::Linear);
            m_image->setTexture(sg);
            appendChildNode(m_image);
        } else {
            m_image->setTexture(sg);
        }
        delete m_texture;
        m_texture = sg;
        m_output = texture;
        m_in = in;
        m_out = out;
        return true;
    }

    QQuickWindow *m_window;
    ID3D11Device *m_device = nullptr; // Qt's reference
    ComPtr<ID3D11VideoDevice> m_videoDevice;
    ComPtr<ID3D11VideoContext1> m_videoContext;
    ComPtr<ID3D11Multithread> m_mt;
    ComPtr<ID3D11VideoProcessorEnumerator> m_enumerator;
    ComPtr<ID3D11VideoProcessor> m_processor;
    ComPtr<ID3D11VideoProcessorOutputView> m_outputView;
    ComPtr<ID3D11Texture2D> m_output;
    std::unordered_map<UINT, ComPtr<ID3D11VideoProcessorInputView>> m_inputViews;
    ID3D11Texture2D *m_poolTexture = nullptr;
    const void *m_pool = nullptr;
    QSize m_in, m_out;
    QSGTexture *m_texture = nullptr;
    QSGImageNode *m_image = nullptr;
};

} // namespace

VideoItem::VideoItem(QQuickItem *parent) : QQuickItem(parent)
{
    setFlag(ItemHasContents);
    liveItems().append(this);
    m_statsTimer.setInterval(1000);
    connect(&m_statsTimer, &QTimer::timeout, this, &VideoItem::sampleStats);
}

VideoItem::~VideoItem()
{
    m_player.reset(); // joins its threads while the item is still whole (the decoder posts to it)
    liveItems().removeOne(this);
}

void VideoItem::setSource(const QString &s)
{
    if (s == m_source)
        return;
    m_source = s;
    m_player.reset();
    m_playerFailed = false;
    m_error.clear();
    emit sourceChanged();
    update();
}

QString VideoItem::codecName() const
{
    return m_player ? QString::fromLatin1(m_player->codecName()) : QString();
}

QSize VideoItem::videoSize() const
{
    return m_player ? QSize(int(m_player->width()), int(m_player->height())) : QSize();
}

int VideoItem::videoFps() const
{
    return m_player ? int(m_player->fps()) : 0;
}

void VideoItem::geometryChange(const QRectF &newGeometry, const QRectF &oldGeometry)
{
    QQuickItem::geometryChange(newGeometry, oldGeometry);
    update();
}

void VideoItem::sampleStats()
{
    if (!m_player)
        return;
    const uint64_t decoded = m_player->decoded, presented = m_presented;
    m_decodedFps = int(decoded - m_lastDecoded);
    m_presentedFps = int(presented - m_lastPresented);
    m_lastDecoded = decoded;
    m_lastPresented = presented;
    emit statsChanged();
}

QSGNode *VideoItem::updatePaintNode(QSGNode *old, UpdatePaintNodeData *)
{
    // Render thread, GUI thread blocked: the item's members are safe to touch.
    if (!m_player && !m_playerFailed && !m_source.isEmpty()) {
        QQuickWindow *w = window();
        auto *device = static_cast<ID3D11Device *>(
            w->rendererInterface()->getResource(w, QSGRendererInterface::DeviceResource));
        if (!device || w->rendererInterface()->graphicsApi() != QSGRendererInterface::Direct3D11) {
            m_error = QStringLiteral("video needs the Direct3D 11 renderer");
            m_playerFailed = true;
        } else {
            QPointer<VideoItem> self(this);
            auto player = std::make_unique<StreamPlayer>(device, m_source, [self] {
                // Decoder thread: ask for a frame on the GUI thread (QQuickItem::update is GUI-thread only).
                QMetaObject::invokeMethod(self, [self] { if (self) self->update(); }, Qt::QueuedConnection);
            });
            if (!player->ok()) {
                m_error = player->error();
                m_playerFailed = true;
            } else {
                m_player = std::move(player);
            }
        }
        QMetaObject::invokeMethod(this, [this] {
            m_statsTimer.start();
            emit statsChanged();
        }, Qt::QueuedConnection);
    }

    auto *node = static_cast<VideoNode *>(old);
    if (!m_player) {
        delete node;
        return nullptr;
    }
    if (!node)
        node = new VideoNode(window());

    const QSize video(int(m_player->width()), int(m_player->height()));
    const QRectF rect = fitRect(boundingRect(), video);
    if (AVFrame *frame = m_player->takeLatest()) {
        // Draw at the on-screen size in device pixels, never above the video's own size.
        const qreal dpr = window()->effectiveDevicePixelRatio();
        const qreal scale = std::min<qreal>(1.0, rect.width() * dpr / frame->width);
        const QSize out(std::max(1, qRound(frame->width * scale)), std::max(1, qRound(frame->height * scale)));
        if (!rect.isEmpty() && node->present(frame, out)) {
            ++m_presented;
            m_player->recordLatency(float(nowNs() - frame->pts) / 1e6f);
        }
        av_frame_free(&frame);
    }
    node->setRect(rect);
    return node;
}

void VideoItem::benchStart()
{
    for (VideoItem *item : liveItems()) {
        if (!item->m_player)
            continue;
        item->m_benchDecoded = item->m_player->decoded;
        item->m_benchPresented = item->m_presented;
        item->m_benchReplaced = item->m_player->replaced;
        item->m_benchFed = item->m_player->fed;
        item->m_player->startSamples();
    }
}

QString VideoItem::benchReport()
{
    auto pct = [](std::vector<float> v, double p) {
        if (v.empty())
            return 0.0;
        std::sort(v.begin(), v.end());
        return double(v[std::min(v.size() - 1, size_t(p * double(v.size())))]);
    };
    QJsonArray out;
    for (VideoItem *item : liveItems()) {
        QJsonObject o;
        o[QStringLiteral("source")] = item->m_source;
        if (!item->m_player) {
            o[QStringLiteral("error")] = item->m_error;
            out.append(o);
            continue;
        }
        StreamPlayer &p = *item->m_player;
        o[QStringLiteral("codec")] = QString::fromLatin1(p.codecName());
        o[QStringLiteral("video")] = QStringLiteral("%1x%2@%3").arg(p.width()).arg(p.height()).arg(p.fps());
        o[QStringLiteral("item_px")] = QStringLiteral("%1x%2").arg(item->width()).arg(item->height());
        o[QStringLiteral("fed")] = qint64(p.fed - item->m_benchFed);
        o[QStringLiteral("decoded")] = qint64(p.decoded - item->m_benchDecoded);
        o[QStringLiteral("presented")] = qint64(item->m_presented - item->m_benchPresented);
        o[QStringLiteral("replaced")] = qint64(p.replaced - item->m_benchReplaced);
        o[QStringLiteral("errors")] = qint64(p.errors);
        o[QStringLiteral("queue_drops")] = qint64(p.queueDrops);
        const auto dec = p.decodeSamples(), done = p.decodeDoneSamples(), lat = p.latencySamples();
        o[QStringLiteral("decode_call_ms_p50")] = pct(dec, 0.5);
        o[QStringLiteral("decode_call_ms_p99")] = pct(dec, 0.99);
        o[QStringLiteral("decode_done_ms_p50")] = pct(done, 0.5);
        o[QStringLiteral("decode_done_ms_p99")] = pct(done, 0.99);
        o[QStringLiteral("feed_to_render_ms_p50")] = pct(lat, 0.5);
        o[QStringLiteral("feed_to_render_ms_p99")] = pct(lat, 0.99);
        out.append(o);
    }
    return QString::fromUtf8(QJsonDocument(out).toJson(QJsonDocument::Compact));
}

} // namespace decibell::video
