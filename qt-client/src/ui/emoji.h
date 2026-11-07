// Twemoji: which code-point sequences have a picture (the file names of assets/twemoji), and the pictures
// rasterised at the size they're drawn. Matches the renderer's components/emoji/Twemoji.tsx.
#pragma once

#include <QCache>
#include <QHash>
#include <QImage>
#include <QString>
#include <QStringView>
#include <vector>

namespace decibell {

class Emoji
{
public:
    static Emoji &instance();

    // Load the sequence index from a folder of `<codepoints>.svg` (idempotent per folder).
    void setDirectory(const QString &dir);

    // The longest twemoji sequence starting at `pos` (UTF-16 index): its length in UTF-16 units and its file key
    // ("1f468-200d-1f469"), or 0. U+FE0F is skipped inside a multi-code-point match (twemoji's file names drop it,
    // as Twemoji.tsx's toCodePoints does); a single text-default symbol (©, ❤, …) needs the FE0F after it.
    qsizetype match(QStringView text, qsizetype pos, QString *key) const;

    // The picture for `key` at `px` device pixels, square. Cached (bounded by bytes); empty if the file is missing.
    QImage image(const QString &key, int px);

private:
    struct Node {
        QHash<char32_t, int> next;
        bool terminal = false;
    };
    std::vector<Node> m_nodes{Node{}};
    QString m_dir;
    QCache<QString, QImage> m_images{16 * 1024 * 1024}; // cost = bytes
};

} // namespace decibell
