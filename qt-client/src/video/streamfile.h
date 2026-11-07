// Reader for the spike's test streams (.dbv, written by tools/streamgen): one encoded frame per record, whole frames
// as the receive path's reassembler hands them on. Streams from disk so a 75 MB file isn't held in memory (and
// doesn't count against the client's working set). Loops at the end: the file starts with a keyframe.
#pragma once

#include <cstdint>
#include <cstdio>
#include <string>

namespace decibell::video {

enum class Codec : uint8_t { H264 = 1, Hevc = 2, Av1 = 3 };

class StreamFile
{
public:
    ~StreamFile()
    {
        if (m_f)
            fclose(m_f);
    }

    bool open(const std::wstring &path)
    {
        if (_wfopen_s(&m_f, path.c_str(), L"rb") != 0 || !m_f)
            return false;
        uint8_t magic[8];
        uint32_t dims[3];
        if (fread(magic, 1, 8, m_f) != 8 || fread(dims, 4, 3, m_f) != 3 || magic[0] != 'D' || magic[1] != 'B'
            || magic[2] != 'V' || magic[3] != '1' || magic[4] < 1 || magic[4] > 3)
            return false;
        codec = Codec(magic[4]);
        width = dims[0];
        height = dims[1];
        fps = dims[2] ? dims[2] : 60;
        return true;
    }

    // The next frame's size and flags; then read() its bytes. Wraps to the first frame at the end.
    bool nextHeader(uint32_t &size, bool &keyframe)
    {
        uint32_t head[2];
        if (fread(head, 4, 2, m_f) != 2) {
            fseek(m_f, kHeaderBytes, SEEK_SET);
            if (fread(head, 4, 2, m_f) != 2)
                return false;
        }
        size = head[0];
        keyframe = head[1] & 1;
        return true;
    }
    bool read(uint8_t *dst, uint32_t size) { return fread(dst, 1, size, m_f) == size; }

    Codec codec = Codec::H264;
    uint32_t width = 0, height = 0, fps = 60;

private:
    static constexpr long kHeaderBytes = 20;
    FILE *m_f = nullptr;
};

} // namespace decibell::video
