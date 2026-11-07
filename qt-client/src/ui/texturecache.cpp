#include "texturecache.h"

#include <QQuickWindow>
#include <QSGTexture>
#include <rhi/qrhi.h>

#include <mutex>
#include <unordered_map>

namespace decibell {

namespace {

std::mutex g_mutex;
std::unordered_map<const QRhi *, std::unordered_map<QString, QSGTexture *>> g_caches;

} // namespace

QSGTexture *cachedTexture(QQuickWindow *window, const QString &key, const std::function<QImage()> &make)
{
    QRhi *rhi = window->rhi();
    {
        std::lock_guard lock(g_mutex);
        auto it = g_caches.find(rhi);
        if (it == g_caches.end()) {
            it = g_caches.emplace(rhi, std::unordered_map<QString, QSGTexture *>{}).first;
            if (rhi) {
                rhi->addCleanupCallback(&g_caches, [](QRhi *dying) {
                    std::lock_guard lock(g_mutex);
                    g_caches.erase(dying);
                });
            }
        }
        if (auto t = it->second.find(key); t != it->second.end())
            return t->second;
    }
    const QImage img = make(); // outside the lock: rasterising can take a moment
    if (img.isNull())
        return nullptr;
    QSGTexture *t = window->createTextureFromImage(img, QQuickWindow::TextureCanUseAtlas);
    std::lock_guard lock(g_mutex);
    g_caches[rhi].emplace(key, t);
    return t;
}

} // namespace decibell
