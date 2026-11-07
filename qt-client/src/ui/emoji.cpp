#include "emoji.h"

#include <QDir>
#include <QPainter>
#include <QSvgRenderer>

namespace decibell {

namespace {

// BMP code points whose default presentation is emoji (Unicode emoji-data.txt, Emoji_Presentation=Yes): these
// count as emoji without a following U+FE0F. Every other single BMP symbol needs it.
bool bmpEmojiPresentation(char32_t c)
{
    static const char32_t ranges[][2] = {
        {0x231A, 0x231B}, {0x23E9, 0x23EC}, {0x23F0, 0x23F0}, {0x23F3, 0x23F3}, {0x25FD, 0x25FE},
        {0x2614, 0x2615}, {0x2648, 0x2653}, {0x267F, 0x267F}, {0x2693, 0x2693}, {0x26A1, 0x26A1},
        {0x26AA, 0x26AB}, {0x26BD, 0x26BE}, {0x26C4, 0x26C5}, {0x26CE, 0x26CE}, {0x26D4, 0x26D4},
        {0x26EA, 0x26EA}, {0x26F2, 0x26F3}, {0x26F5, 0x26F5}, {0x26FA, 0x26FA}, {0x26FD, 0x26FD},
        {0x2705, 0x2705}, {0x270A, 0x270B}, {0x2728, 0x2728}, {0x274C, 0x274C}, {0x274E, 0x274E},
        {0x2753, 0x2755}, {0x2757, 0x2757}, {0x2795, 0x2797}, {0x27B0, 0x27B0}, {0x27BF, 0x27BF},
        {0x2B1B, 0x2B1C}, {0x2B50, 0x2B50}, {0x2B55, 0x2B55},
    };
    for (const auto &r : ranges)
        if (c >= r[0] && c <= r[1])
            return true;
    return false;
}

char32_t codePointAt(QStringView s, qsizetype i, qsizetype *len)
{
    const QChar c = s[i];
    if (c.isHighSurrogate() && i + 1 < s.size() && s[i + 1].isLowSurrogate()) {
        *len = 2;
        return QChar::surrogateToUcs4(c, s[i + 1]);
    }
    *len = 1;
    return c.unicode();
}

} // namespace

Emoji &Emoji::instance()
{
    static Emoji e;
    return e;
}

void Emoji::setDirectory(const QString &dir)
{
    if (dir == m_dir)
        return;
    m_dir = dir;
    m_nodes.assign(1, Node{});
    m_images.clear();
    const QStringList files = QDir(dir).entryList({QStringLiteral("*.svg")}, QDir::Files);
    for (const QString &f : files) {
        int node = 0;
        for (QStringView part : QStringView(f).chopped(4).split(u'-')) {
            bool ok = false;
            const char32_t cp = char32_t(part.toUInt(&ok, 16));
            if (!ok)
                break;
            auto it = m_nodes[node].next.constFind(cp);
            if (it == m_nodes[node].next.cend()) {
                m_nodes.push_back(Node{});
                const int created = int(m_nodes.size() - 1);
                m_nodes[node].next.insert(cp, created);
                node = created;
            } else {
                node = *it;
            }
        }
        m_nodes[node].terminal = true;
    }
}

qsizetype Emoji::match(QStringView text, qsizetype pos, QString *key) const
{
    int node = 0;
    qsizetype i = pos;
    qsizetype bestEnd = 0;
    int bestCount = 0;
    int count = 0;
    QString seq;
    QString bestSeq;
    while (i < text.size()) {
        qsizetype len = 0;
        const char32_t cp = codePointAt(text, i, &len);
        if (cp == 0xFE0F && count > 0) { // variation selector inside a sequence: consumed, not in the file name
            i += len;
            if (node && m_nodes[node].terminal && bestEnd == i - len) {
                bestEnd = i;
            }
            continue;
        }
        auto it = m_nodes[node].next.constFind(cp);
        if (it == m_nodes[node].next.cend())
            break;
        node = *it;
        i += len;
        ++count;
        if (!seq.isEmpty())
            seq += u'-';
        seq += QString::number(uint(cp), 16);
        if (m_nodes[node].terminal) {
            bestEnd = i;
            bestCount = count;
            bestSeq = seq;
            // A trailing FE0F belongs to this emoji.
            qsizetype l2 = 0;
            if (i < text.size() && codePointAt(text, i, &l2) == 0xFE0F)
                bestEnd = i + l2;
        }
    }
    if (!bestEnd)
        return 0;
    if (bestCount == 1) {
        // One code point: a text-default BMP symbol is an emoji only with FE0F after it.
        qsizetype l = 0;
        const char32_t first = codePointAt(text, pos, &l);
        const bool hasVs = bestEnd > pos + l;
        if (first < 0x10000 && !bmpEmojiPresentation(first) && !hasVs)
            return 0;
    }
    if (key)
        *key = bestSeq;
    return bestEnd - pos;
}

QImage Emoji::image(const QString &key, int px)
{
    const QString cacheKey = key + u'@' + QString::number(px);
    if (QImage *hit = m_images.object(cacheKey))
        return *hit;
    QSvgRenderer svg(m_dir + u'/' + key + u".svg");
    if (!svg.isValid())
        return {};
    QImage img(px, px, QImage::Format_ARGB32_Premultiplied);
    img.fill(Qt::transparent);
    QPainter p(&img);
    p.setRenderHint(QPainter::Antialiasing);
    svg.render(&p, QRectF(0, 0, px, px));
    p.end();
    m_images.insert(cacheKey, new QImage(img), int(img.sizeInBytes()));
    return img;
}

} // namespace decibell
